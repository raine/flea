use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use image::{
    ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat,
    ImageReader, Limits,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType, PngEncoder},
    },
    imageops::FilterType as ResizeFilter,
    metadata::{
        Cicp, CicpColorPrimaries, CicpMatrixCoefficients, CicpTransferCharacteristics,
        CicpVideoFullRangeFlag, Orientation,
    },
};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::{Builder as TempBuilder, TempDir};
use wait_timeout::ChildExt;

const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 10_000;
const MAX_SOURCE_PIXELS: u64 = 50_000_000;
const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COLOR_PROFILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FINAL_DIMENSION: u32 = 2_560;
const MAX_FINAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_INTERMEDIATE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PROCESSING_TIME: Duration = Duration::from_secs(30);
const MAX_DECODER_TIME: Duration = Duration::from_secs(12);
#[cfg(not(target_os = "macos"))]
const DECODER_ADDRESS_SPACE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageProcessingReport {
    pub source_format: String,
    pub uploaded_format: String,
    pub final_width: u32,
    pub final_height: u32,
    pub final_byte_size: u64,
    pub metadata_stripped: bool,
    pub recompressed: bool,
}

#[derive(Debug)]
pub(crate) struct ProcessedImage {
    pub bytes: Vec<u8>,
    pub file_name: &'static str,
    pub width: u32,
    pub height: u32,
    pub report: ImageProcessingReport,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProcessingError {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ProcessingError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub(crate) fn preprocess_path(path: &Path) -> Result<ProcessedImage, ProcessingError> {
    let path = path.to_owned();
    run_bounded(MAX_PROCESSING_TIME, move |context| {
        context.enter(ProcessingStage::Read)?;
        let bytes = read_source(&path)?;
        preprocess_inner(bytes, None, &context)
    })
}

pub(crate) fn preprocess_bytes(bytes: Vec<u8>) -> Result<ProcessedImage, ProcessingError> {
    validate_source_size(bytes.len())?;
    run_bounded(MAX_PROCESSING_TIME, move |context| {
        preprocess_inner(bytes, None, &context)
    })
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum ProcessingStage {
    Starting,
    Read,
    HeicDecode,
    RasterDecode,
    Orientation,
    ColorNormalization,
    Resize,
    Encode,
}

impl ProcessingStage {
    const fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Read => "read",
            Self::HeicDecode => "heic_decode",
            Self::RasterDecode => "raster_decode",
            Self::Orientation => "orientation",
            Self::ColorNormalization => "color_normalization",
            Self::Resize => "resize",
            Self::Encode => "encode",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Read,
            2 => Self::HeicDecode,
            3 => Self::RasterDecode,
            4 => Self::Orientation,
            5 => Self::ColorNormalization,
            6 => Self::Resize,
            7 => Self::Encode,
            _ => Self::Starting,
        }
    }
}

struct ProcessingContext {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    stage: Arc<AtomicU8>,
}

impl ProcessingContext {
    fn enter(&self, stage: ProcessingStage) -> Result<(), ProcessingError> {
        self.stage.store(stage as u8, Ordering::Release);
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            Err(processing_timeout(stage))
        } else {
            Ok(())
        }
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

fn run_bounded(
    timeout: Duration,
    operation: impl FnOnce(ProcessingContext) -> Result<ProcessedImage, ProcessingError>
    + Send
    + 'static,
) -> Result<ProcessedImage, ProcessingError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let stage = Arc::new(AtomicU8::new(ProcessingStage::Starting as u8));
    let context = ProcessingContext {
        deadline: Instant::now() + timeout,
        cancelled: Arc::clone(&cancelled),
        stage: Arc::clone(&stage),
    };
    std::thread::Builder::new()
        .name("flea-image-processing".to_owned())
        .spawn(move || {
            let _ = sender.send(operation(context));
        })
        .map_err(|_| processing_failed())?;

    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            cancelled.store(true, Ordering::Release);
            Err(processing_timeout(ProcessingStage::from_u8(
                stage.load(Ordering::Acquire),
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(processing_failed()),
    }
}

fn processing_timeout(stage: ProcessingStage) -> ProcessingError {
    ProcessingError::new(
        "draft.image_processing_timeout",
        format!(
            "Image preprocessing stage '{}' exceeded the 30-second safety limit",
            stage.name()
        ),
    )
    .with_details(json!({ "stage": stage.name() }))
}

fn read_source(path: &Path) -> Result<Vec<u8>, ProcessingError> {
    let mut file = File::open(path).map_err(|error| {
        ProcessingError::new(
            "draft.image_read_failed",
            format!("Failed to read image: {error}"),
        )
    })?;
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if length > MAX_SOURCE_BYTES {
        return Err(source_too_large(length));
    }

    let capacity = usize::try_from(length.min(MAX_SOURCE_BYTES)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ProcessingError::new(
                "draft.image_read_failed",
                format!("Failed to read image: {error}"),
            )
        })?;
    validate_source_size(bytes.len())?;
    Ok(bytes)
}

fn validate_source_size(length: usize) -> Result<(), ProcessingError> {
    if u64::try_from(length).unwrap_or(u64::MAX) > MAX_SOURCE_BYTES {
        Err(source_too_large(u64::try_from(length).unwrap_or(u64::MAX)))
    } else {
        Ok(())
    }
}

fn source_too_large(length: u64) -> ProcessingError {
    ProcessingError::new(
        "draft.image_source_too_large",
        "Image input exceeds the 32 MiB safety limit",
    )
    .with_details(json!({
        "source_byte_size": length,
        "maximum_source_byte_size": MAX_SOURCE_BYTES
    }))
}

fn preprocess_inner(
    bytes: Vec<u8>,
    decoder_settings: Option<&DecoderSettings<'_>>,
    context: &ProcessingContext,
) -> Result<ProcessedImage, ProcessingError> {
    match detect_source_format(&bytes)? {
        SourceFormat::Jpeg => {
            process_raster(bytes, SourceFormat::Jpeg, ImageFormat::Jpeg, false, context)
        }
        SourceFormat::Png => {
            process_raster(bytes, SourceFormat::Png, ImageFormat::Png, false, context)
        }
        source @ (SourceFormat::Heic | SourceFormat::Heif) => {
            context.enter(ProcessingStage::HeicDecode)?;
            let decoded = decode_heif(bytes, decoder_settings, context)?;
            process_raster(decoded, source, ImageFormat::Png, true, context)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceFormat {
    Jpeg,
    Png,
    Heic,
    Heif,
}

impl SourceFormat {
    const fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Heic => "heic",
            Self::Heif => "heif",
        }
    }
}

fn detect_source_format(bytes: &[u8]) -> Result<SourceFormat, ProcessingError> {
    match image::guess_format(bytes) {
        Ok(ImageFormat::Jpeg) => return Ok(SourceFormat::Jpeg),
        Ok(ImageFormat::Png) => return Ok(SourceFormat::Png),
        Ok(_) => return Err(unsupported_format()),
        Err(_) => {}
    }

    let brands = heif_brands(bytes).ok_or_else(unsupported_format)?;
    if brands
        .iter()
        .any(|brand| **brand == *b"avif" || **brand == *b"avis")
    {
        return Err(unsupported_format());
    }
    if brands.iter().any(|brand| {
        matches!(
            *brand,
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs"
        )
    }) {
        return Ok(SourceFormat::Heic);
    }
    if brands
        .iter()
        .any(|brand| **brand == *b"mif1" || **brand == *b"msf1")
    {
        return Ok(SourceFormat::Heif);
    }
    Err(unsupported_format())
}

fn heif_brands(bytes: &[u8]) -> Option<Vec<&[u8; 4]>> {
    if bytes.len() < 16 || bytes.get(4..8)? != b"ftyp" {
        return None;
    }
    let box_size = usize::try_from(u32::from_be_bytes(bytes.get(0..4)?.try_into().ok()?)).ok()?;
    if box_size < 16 || box_size > bytes.len() || (box_size - 16) % 4 != 0 {
        return None;
    }
    let mut brands = vec![bytes.get(8..12)?.try_into().ok()?];
    for brand in bytes.get(16..box_size)?.chunks_exact(4) {
        brands.push(brand.try_into().ok()?);
    }
    Some(brands)
}

fn unsupported_format() -> ProcessingError {
    ProcessingError::new(
        "draft.invalid_image",
        "Image must be JPEG, PNG, HEIC, or HEIF",
    )
}

fn process_raster(
    bytes: Vec<u8>,
    source_format: SourceFormat,
    raster_format: ImageFormat,
    force_reencode: bool,
    context: &ProcessingContext,
) -> Result<ProcessedImage, ProcessingError> {
    let png_metadata = (raster_format == ImageFormat::Png).then(|| inspect_png(&bytes));
    let raw_is_safe = match raster_format {
        ImageFormat::Jpeg => jpeg_has_only_safe_segments(&bytes),
        ImageFormat::Png => png_metadata.as_ref().is_some_and(|metadata| metadata.safe),
        _ => false,
    };

    context.enter(ProcessingStage::RasterDecode)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), raster_format);
    let mut initial_limits = Limits::default();
    initial_limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(initial_limits);
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    let (source_width, source_height) = decoder.dimensions();
    validate_dimensions(source_width, source_height, decoder.total_bytes())?;
    let mut decode_limits = Limits::default();
    decode_limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    decode_limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    decode_limits.max_alloc = Some(MAX_DECODED_BYTES);
    decoder.set_limits(decode_limits).map_err(decode_error)?;

    let orientation = decoder.orientation().map_err(decode_error)?;
    let color_profile = decoder.icc_profile().map_err(decode_error)?;
    if color_profile
        .as_ref()
        .is_some_and(|profile| profile.len() > MAX_COLOR_PROFILE_BYTES)
    {
        return Err(ProcessingError::new(
            "draft.image_metadata_too_large",
            "Image color metadata exceeds the 4 MiB safety limit",
        ));
    }
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;

    let within_output_bounds = source_width <= MAX_FINAL_DIMENSION
        && source_height <= MAX_FINAL_DIMENSION
        && bytes.len() <= MAX_FINAL_BYTES;
    if !force_reencode
        && raw_is_safe
        && orientation == Orientation::NoTransforms
        && within_output_bounds
    {
        return Ok(finish(
            bytes,
            raster_format,
            source_format,
            source_width,
            source_height,
            false,
        ));
    }

    context.enter(ProcessingStage::Orientation)?;
    image.apply_orientation(orientation);
    context.enter(ProcessingStage::ColorNormalization)?;
    normalize_color(
        &mut image,
        color_profile.as_deref(),
        png_metadata.and_then(|metadata| metadata.cicp),
    );
    if image.width() > MAX_FINAL_DIMENSION || image.height() > MAX_FINAL_DIMENSION {
        context.enter(ProcessingStage::Resize)?;
        image = image.resize(
            MAX_FINAL_DIMENSION,
            MAX_FINAL_DIMENSION,
            ResizeFilter::Lanczos3,
        );
    }

    let output_format = if image.color().has_alpha() {
        ImageFormat::Png
    } else if raster_format == ImageFormat::Jpeg || force_reencode {
        ImageFormat::Jpeg
    } else {
        ImageFormat::Png
    };
    context.enter(ProcessingStage::Encode)?;
    let (encoded, width, height) = encode_bounded(image, output_format, context)?;
    Ok(finish(
        encoded,
        output_format,
        source_format,
        width,
        height,
        true,
    ))
}

fn validate_dimensions(width: u32, height: u32, decoded_bytes: u64) -> Result<(), ProcessingError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SOURCE_DIMENSION
        || height > MAX_SOURCE_DIMENSION
        || pixels > MAX_SOURCE_PIXELS
    {
        return Err(ProcessingError::new(
            "draft.image_dimensions_exceeded",
            "Image dimensions exceed the decode safety policy",
        )
        .with_details(json!({
            "width": width,
            "height": height,
            "maximum_dimension": MAX_SOURCE_DIMENSION,
            "maximum_pixels": MAX_SOURCE_PIXELS
        })));
    }
    if decoded_bytes > MAX_DECODED_BYTES {
        return Err(ProcessingError::new(
            "draft.image_memory_limit",
            "Decoded image exceeds the 256 MiB memory safety limit",
        ));
    }
    Ok(())
}

fn decode_error(error: image::ImageError) -> ProcessingError {
    if matches!(error, image::ImageError::Limits(_)) {
        ProcessingError::new(
            "draft.image_memory_limit",
            "Image exceeds the decode safety limits",
        )
    } else {
        ProcessingError::new(
            "draft.invalid_image",
            "Image data is malformed or unsupported",
        )
    }
}

fn normalize_color(image: &mut DynamicImage, icc: Option<&[u8]>, cicp: Option<Cicp>) {
    if let Some(icc) = icc
        && convert_icc_to_srgb(image, icc)
    {
        return;
    }
    if let Some(cicp) = cicp {
        let original = image.clone();
        if image.set_color_space(cicp).is_err()
            || image
                .apply_color_space(Cicp::SRGB, Default::default())
                .is_err()
        {
            *image = original;
        }
    }
}

fn convert_icc_to_srgb(image: &mut DynamicImage, profile_bytes: &[u8]) -> bool {
    let Ok(profile) = ColorProfile::new_from_slice(profile_bytes) else {
        return false;
    };
    if profile.color_space != DataColorSpace::Rgb {
        return false;
    }
    let srgb = ColorProfile::new_srgb();
    let (width, height) = image.dimensions();
    if image.color().has_alpha() {
        let source = image.to_rgba8().into_raw();
        let mut converted = vec![0; source.len()];
        let Ok(transform) = profile.create_transform_8bit(
            Layout::Rgba,
            &srgb,
            Layout::Rgba,
            TransformOptions::default(),
        ) else {
            return false;
        };
        if transform.transform(&source, &mut converted).is_err() {
            return false;
        }
        let Some(buffer) = image::RgbaImage::from_raw(width, height, converted) else {
            return false;
        };
        *image = DynamicImage::ImageRgba8(buffer);
    } else {
        let source = image.to_rgb8().into_raw();
        let mut converted = vec![0; source.len()];
        let Ok(transform) = profile.create_transform_8bit(
            Layout::Rgb,
            &srgb,
            Layout::Rgb,
            TransformOptions::default(),
        ) else {
            return false;
        };
        if transform.transform(&source, &mut converted).is_err() {
            return false;
        }
        let Some(buffer) = image::RgbImage::from_raw(width, height, converted) else {
            return false;
        };
        *image = DynamicImage::ImageRgb8(buffer);
    }
    true
}

fn encode_bounded(
    mut image: DynamicImage,
    format: ImageFormat,
    context: &ProcessingContext,
) -> Result<(Vec<u8>, u32, u32), ProcessingError> {
    loop {
        context.enter(ProcessingStage::Encode)?;
        let encoded = match format {
            ImageFormat::Jpeg => encode_jpeg(&image)?,
            ImageFormat::Png => encode_png(&image)?,
            _ => unreachable!("output format is fixed by policy"),
        };
        if encoded.len() <= MAX_FINAL_BYTES {
            return Ok((encoded, image.width(), image.height()));
        }
        if image.width() == 1 && image.height() == 1 {
            return Err(ProcessingError::new(
                "draft.image_encode_failed",
                "Image could not be encoded within the 8 MiB upload limit",
            ));
        }
        let width = (image.width().saturating_mul(3) / 4).max(1);
        let height = (image.height().saturating_mul(3) / 4).max(1);
        context.enter(ProcessingStage::Resize)?;
        image = image.resize_exact(width, height, ResizeFilter::Lanczos3);
    }
}

fn encode_jpeg(image: &DynamicImage) -> Result<Vec<u8>, ProcessingError> {
    const QUALITIES: [u8; 4] = [88, 80, 72, 64];
    let rgb = image.to_rgb8();
    let mut last = Vec::new();
    for quality in QUALITIES {
        let mut output = Vec::new();
        JpegEncoder::new_with_quality(&mut output, quality)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ColorType::Rgb8.into(),
            )
            .map_err(encode_error)?;
        if output.len() <= MAX_FINAL_BYTES {
            return Ok(output);
        }
        last = output;
    }
    Ok(last)
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, ProcessingError> {
    let mut output = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut output, CompressionType::Default, FilterType::Adaptive);
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        encoder
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ColorType::Rgba8.into(),
            )
            .map_err(encode_error)?;
    } else {
        let rgb = image.to_rgb8();
        encoder
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ColorType::Rgb8.into(),
            )
            .map_err(encode_error)?;
    }
    Ok(output)
}

fn encode_error(_error: image::ImageError) -> ProcessingError {
    ProcessingError::new(
        "draft.image_encode_failed",
        "Image conversion to an upload-safe format failed",
    )
}

fn finish(
    bytes: Vec<u8>,
    format: ImageFormat,
    source_format: SourceFormat,
    width: u32,
    height: u32,
    recompressed: bool,
) -> ProcessedImage {
    let (file_name, uploaded_format) = match format {
        ImageFormat::Jpeg => ("image.jpg", "jpeg"),
        ImageFormat::Png => ("image.png", "png"),
        _ => unreachable!("output format is fixed by policy"),
    };
    ProcessedImage {
        report: ImageProcessingReport {
            source_format: source_format.name().to_owned(),
            uploaded_format: uploaded_format.to_owned(),
            final_width: width,
            final_height: height,
            final_byte_size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            metadata_stripped: true,
            recompressed,
        },
        bytes,
        file_name,
        width,
        height,
    }
}

#[derive(Clone, Copy)]
enum DecoderKind {
    Libheif,
    Sips,
}

struct DecoderProgram {
    executable: PathBuf,
    kind: DecoderKind,
}

struct DecoderSettings<'a> {
    programs: &'a [DecoderProgram],
    temp_root: Option<&'a Path>,
    timeout: Duration,
}

fn decode_heif(
    bytes: Vec<u8>,
    settings: Option<&DecoderSettings<'_>>,
    context: &ProcessingContext,
) -> Result<Vec<u8>, ProcessingError> {
    let default_programs = default_decoder_programs();
    let programs = settings.map_or(default_programs.as_slice(), |settings| settings.programs);
    let temp_root = settings.and_then(|settings| settings.temp_root);
    let timeout = settings.map_or(MAX_DECODER_TIME, |settings| settings.timeout);
    let workspace = TempWorkspace::new(temp_root)?;
    workspace.write_input(&bytes)?;
    drop(bytes);

    let mut found_decoder = false;
    let started = Instant::now();
    for program in programs {
        workspace.clear_output()?;
        context.enter(ProcessingStage::HeicDecode)?;
        let decoder_remaining = timeout.saturating_sub(started.elapsed());
        let total_remaining = context.remaining();
        let remaining = decoder_remaining.min(total_remaining);
        if remaining.is_zero() {
            return if total_remaining.is_zero() {
                Err(processing_timeout(ProcessingStage::HeicDecode))
            } else {
                Err(heic_decode_timeout())
            };
        }
        match run_decoder(program, &workspace, remaining, MAX_FINAL_DIMENSION) {
            DecoderRun::Unavailable => continue,
            DecoderRun::Rejected(error) => return Err(error),
            DecoderRun::TimedOut => {
                return if context.remaining().is_zero() {
                    Err(processing_timeout(ProcessingStage::HeicDecode))
                } else {
                    Err(heic_decode_timeout())
                };
            }
            DecoderRun::Failed => found_decoder = true,
            DecoderRun::Succeeded => {
                workspace.make_output_private()?;
                match workspace.read_output() {
                    Ok(output) => return Ok(output),
                    Err(error) if error.code == "draft.heic_decode_failed" => {
                        found_decoder = true;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    if found_decoder {
        Err(ProcessingError::new(
            "draft.heic_decode_failed",
            "HEIC/HEIF decoding failed; verify that libheif includes an HEVC decoder and that the image is valid",
        ))
    } else {
        Err(ProcessingError::new(
            "draft.heic_decoder_unavailable",
            "HEIC/HEIF decoding requires `heif-convert` from libheif, or macOS `sips`; install libheif with your platform package manager",
        ))
    }
}

fn heic_decode_timeout() -> ProcessingError {
    ProcessingError::new(
        "draft.heic_decode_timeout",
        "HEIC/HEIF decoding exceeded the bounded safety timeout",
    )
    .with_details(json!({ "stage": ProcessingStage::HeicDecode.name() }))
}

fn default_decoder_programs() -> Vec<DecoderProgram> {
    #[cfg(target_os = "macos")]
    return vec![
        DecoderProgram {
            executable: PathBuf::from("/usr/bin/sips"),
            kind: DecoderKind::Sips,
        },
        DecoderProgram {
            executable: PathBuf::from("heif-convert"),
            kind: DecoderKind::Libheif,
        },
    ];
    #[cfg(not(target_os = "macos"))]
    vec![DecoderProgram {
        executable: PathBuf::from("heif-convert"),
        kind: DecoderKind::Libheif,
    }]
}

struct TempWorkspace {
    directory: TempDir,
    input: PathBuf,
    output: PathBuf,
}

impl TempWorkspace {
    fn new(root: Option<&Path>) -> Result<Self, ProcessingError> {
        let directory = match root {
            Some(root) => TempBuilder::new().prefix("flea-image-").tempdir_in(root),
            None => TempBuilder::new().prefix("flea-image-").tempdir(),
        }
        .map_err(temp_error)?;
        set_directory_private(directory.path())?;
        let input = directory.path().join("input.heic");
        let output = directory.path().join("decoded.png");
        create_private_file(&input)?;
        create_private_file(&output)?;
        Ok(Self {
            directory,
            input,
            output,
        })
    }

    fn write_input(&self, bytes: &[u8]) -> Result<(), ProcessingError> {
        let mut file = private_open(&self.input, true)?;
        file.write_all(bytes).map_err(temp_error)
    }

    fn clear_output(&self) -> Result<(), ProcessingError> {
        private_open(&self.output, true).map(|_| ())
    }

    fn make_output_private(&self) -> Result<(), ProcessingError> {
        set_file_private(&self.output)
    }

    fn read_output(&self) -> Result<Vec<u8>, ProcessingError> {
        let metadata = fs::metadata(&self.output).map_err(temp_error)?;
        if metadata.len() == 0 || metadata.len() > MAX_INTERMEDIATE_BYTES {
            return Err(ProcessingError::new(
                "draft.heic_decode_failed",
                "HEIC/HEIF decoder produced invalid or oversized image data",
            ));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        File::open(&self.output)
            .map_err(temp_error)?
            .take(MAX_INTERMEDIATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(temp_error)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_INTERMEDIATE_BYTES {
            return Err(ProcessingError::new(
                "draft.heic_decode_failed",
                "HEIC/HEIF decoder produced oversized image data",
            ));
        }
        Ok(bytes)
    }
}

fn temp_error(_error: std::io::Error) -> ProcessingError {
    ProcessingError::new(
        "draft.image_temporary_file_failed",
        "Could not create private temporary image storage",
    )
}

#[cfg(unix)]
fn set_directory_private(path: &Path) -> Result<(), ProcessingError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(temp_error)
}

#[cfg(not(unix))]
fn set_directory_private(_path: &Path) -> Result<(), ProcessingError> {
    Ok(())
}

fn create_private_file(path: &Path) -> Result<(), ProcessingError> {
    private_open(path, true).map(|_| ())
}

fn private_open(path: &Path, truncate: bool) -> Result<File, ProcessingError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(temp_error)?;
    set_file_private(path)?;
    Ok(file)
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<(), ProcessingError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(temp_error)
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<(), ProcessingError> {
    Ok(())
}

enum DecoderRun {
    Succeeded,
    Failed,
    TimedOut,
    Unavailable,
    Rejected(ProcessingError),
}

fn run_decoder(
    program: &DecoderProgram,
    workspace: &TempWorkspace,
    timeout: Duration,
    maximum_dimension: u32,
) -> DecoderRun {
    let started = Instant::now();
    let sips_dimensions = if matches!(program.kind, DecoderKind::Sips) {
        match query_sips_dimensions(program, workspace, timeout) {
            Ok(dimensions) => Some(dimensions),
            Err(result) => return result,
        }
    } else {
        None
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return DecoderRun::TimedOut;
    }

    let mut command = Command::new(&program.executable);
    match program.kind {
        DecoderKind::Libheif => {
            command.arg(&workspace.input).arg(&workspace.output);
        }
        DecoderKind::Sips => {
            command.args(["-s", "format", "png"]);
            if sips_dimensions.is_some_and(|(width, height)| {
                width > maximum_dimension || height > maximum_dimension
            }) {
                command
                    .arg("--resampleHeightWidthMax")
                    .arg(maximum_dimension.to_string());
            }
            command
                .arg(&workspace.input)
                .arg("--out")
                .arg(&workspace.output);
        }
    }
    configure_decoder_command(&mut command, workspace, false);
    wait_for_decoder(command, remaining)
}

fn query_sips_dimensions(
    program: &DecoderProgram,
    workspace: &TempWorkspace,
    timeout: Duration,
) -> Result<(u32, u32), DecoderRun> {
    let mut command = Command::new(&program.executable);
    command
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(&workspace.input);
    configure_decoder_command(&mut command, workspace, true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(DecoderRun::Unavailable);
        }
        Err(_) => return Err(DecoderRun::Failed),
    };
    match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(_)) => return Err(DecoderRun::Failed),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DecoderRun::TimedOut);
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DecoderRun::Failed);
        }
    }
    let mut output = String::new();
    if child
        .stdout
        .take()
        .is_none_or(|stdout| stdout.take(4_096).read_to_string(&mut output).is_err())
    {
        return Err(DecoderRun::Failed);
    }
    let width = sips_property(&output, "pixelWidth").ok_or(DecoderRun::Failed)?;
    let height = sips_property(&output, "pixelHeight").ok_or(DecoderRun::Failed)?;
    validate_dimensions(width, height, u64::from(width) * u64::from(height) * 4)
        .map_err(DecoderRun::Rejected)?;
    Ok((width, height))
}

fn sips_property(output: &str, property: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let (name, value) = line.trim().split_once(':')?;
        (name == property)
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn configure_decoder_command(
    command: &mut Command,
    workspace: &TempWorkspace,
    capture_stdout: bool,
) {
    command
        .current_dir(workspace.directory.path())
        .stdin(Stdio::null())
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());
    apply_process_limits(command);
}

fn wait_for_decoder(mut command: Command, timeout: Duration) -> DecoderRun {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DecoderRun::Unavailable;
        }
        Err(_) => return DecoderRun::Failed,
    };
    match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => DecoderRun::Succeeded,
        Ok(Some(_)) => DecoderRun::Failed,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            DecoderRun::TimedOut
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            DecoderRun::Failed
        }
    }
}

#[cfg(unix)]
fn apply_process_limits(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // umask and setrlimit are async-signal-safe and the child closure performs no allocation.
    unsafe {
        command.pre_exec(set_decoder_resource_limits);
    }
}

#[cfg(unix)]
fn set_decoder_resource_limits() -> std::io::Result<()> {
    unsafe {
        libc::umask(0o077);
    }
    let cpu = libc::rlimit {
        rlim_cur: MAX_DECODER_TIME.as_secs() + 1,
        rlim_max: MAX_DECODER_TIME.as_secs() + 1,
    };
    let file = libc::rlimit {
        rlim_cur: MAX_INTERMEDIATE_BYTES,
        rlim_max: MAX_INTERMEDIATE_BYTES,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &cpu) } != 0
        || unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &file) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let memory = libc::rlimit {
            rlim_cur: DECODER_ADDRESS_SPACE_BYTES,
            rlim_max: DECODER_ADDRESS_SPACE_BYTES,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &memory) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_process_limits(_command: &mut Command) {}

fn processing_failed() -> ProcessingError {
    ProcessingError::new(
        "draft.image_processing_failed",
        "Image preprocessing failed unexpectedly",
    )
}

fn jpeg_has_only_safe_segments(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut offset = 2;
    let mut in_scan = false;
    loop {
        if in_scan {
            let Some(marker_offset) = next_jpeg_scan_marker(bytes, offset) else {
                return false;
            };
            offset = marker_offset;
            in_scan = false;
        }
        if offset >= bytes.len() || bytes[offset] != 0xff {
            return false;
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let Some(&marker) = bytes.get(offset) else {
            return false;
        };
        offset += 1;
        match marker {
            0xd9 => return offset == bytes.len(),
            0xd8 | 0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }
        let Some(length_bytes) = bytes.get(offset..offset + 2) else {
            return false;
        };
        let length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if length < 2 {
            return false;
        }
        let segment_end = match offset.checked_add(length) {
            Some(end) if end <= bytes.len() => end,
            _ => return false,
        };
        let payload = &bytes[offset + 2..segment_end];
        match marker {
            0xe0 if safe_jfif(payload) => {}
            0xe0..=0xef | 0xfe => return false,
            0xda => in_scan = true,
            _ => {}
        }
        offset = segment_end;
    }
}

fn next_jpeg_scan_marker(bytes: &[u8], mut offset: usize) -> Option<usize> {
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let mut next = offset + 1;
        while bytes.get(next) == Some(&0xff) {
            next += 1;
        }
        match bytes.get(next).copied()? {
            0x00 => offset = next + 1,
            0xd0..=0xd7 => offset = next + 1,
            _ => return Some(offset),
        }
    }
    None
}

fn safe_jfif(payload: &[u8]) -> bool {
    payload.len() == 14
        && payload.starts_with(b"JFIF\0")
        && payload.get(12) == Some(&0)
        && payload.get(13) == Some(&0)
}

#[derive(Clone, Copy, Default)]
struct PngMetadata {
    safe: bool,
    cicp: Option<Cicp>,
}

fn inspect_png(bytes: &[u8]) -> PngMetadata {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return PngMetadata::default();
    }
    let mut offset = 8;
    let mut safe = true;
    let mut cicp = None;
    while offset + 12 <= bytes.len() {
        let length = match bytes
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_be_bytes)
            .and_then(|value| usize::try_from(value).ok())
        {
            Some(length) => length,
            None => return PngMetadata::default(),
        };
        let kind = &bytes[offset + 4..offset + 8];
        let end = match offset
            .checked_add(12)
            .and_then(|base| base.checked_add(length))
        {
            Some(end) if end <= bytes.len() => end,
            _ => return PngMetadata::default(),
        };
        let payload = &bytes[offset + 8..offset + 8 + length];
        if kind == b"cICP" {
            cicp = parse_cicp(payload);
        }
        if !matches!(kind, b"IHDR" | b"PLTE" | b"IDAT" | b"IEND" | b"tRNS") {
            safe = false;
        }
        offset = end;
        if kind == b"IEND" {
            return PngMetadata {
                safe: safe && length == 0 && offset == bytes.len(),
                cicp,
            };
        }
    }
    PngMetadata::default()
}

fn parse_cicp(payload: &[u8]) -> Option<Cicp> {
    let [primaries, transfer, matrix, full_range] = *<&[u8; 4]>::try_from(payload).ok()?;
    Some(Cicp {
        primaries: match primaries {
            1 => CicpColorPrimaries::SRgb,
            4 => CicpColorPrimaries::RgbM,
            5 => CicpColorPrimaries::RgbB,
            6 => CicpColorPrimaries::Bt601,
            7 => CicpColorPrimaries::Rgb240m,
            8 => CicpColorPrimaries::GenericFilm,
            9 => CicpColorPrimaries::Rgb2020,
            10 => CicpColorPrimaries::Xyz,
            11 => CicpColorPrimaries::SmpteRp431,
            12 => CicpColorPrimaries::SmpteRp432,
            22 => CicpColorPrimaries::Industry22,
            _ => return None,
        },
        transfer: match transfer {
            1 => CicpTransferCharacteristics::Bt709,
            4 => CicpTransferCharacteristics::Bt470M,
            5 => CicpTransferCharacteristics::Bt470BG,
            6 => CicpTransferCharacteristics::Bt601,
            7 => CicpTransferCharacteristics::Smpte240m,
            8 => CicpTransferCharacteristics::Linear,
            9 => CicpTransferCharacteristics::Log100,
            10 => CicpTransferCharacteristics::LogSqrt,
            11 => CicpTransferCharacteristics::Iec61966_2_4,
            12 => CicpTransferCharacteristics::Bt1361,
            13 => CicpTransferCharacteristics::SRgb,
            14 => CicpTransferCharacteristics::Bt2020_10bit,
            15 => CicpTransferCharacteristics::Bt2020_12bit,
            16 => CicpTransferCharacteristics::Smpte2084,
            17 => CicpTransferCharacteristics::Smpte428,
            18 => CicpTransferCharacteristics::Bt2100Hlg,
            _ => return None,
        },
        matrix: match matrix {
            0 => CicpMatrixCoefficients::Identity,
            1 => CicpMatrixCoefficients::Bt709,
            4 => CicpMatrixCoefficients::UsFCC,
            5 => CicpMatrixCoefficients::Bt470BG,
            6 => CicpMatrixCoefficients::Smpte170m,
            7 => CicpMatrixCoefficients::Smpte240m,
            8 => CicpMatrixCoefficients::YCgCo,
            9 => CicpMatrixCoefficients::Bt2020NonConstant,
            10 => CicpMatrixCoefficients::Bt2020Constant,
            11 => CicpMatrixCoefficients::Smpte2085,
            12 => CicpMatrixCoefficients::ChromaticityDerivedNonConstant,
            13 => CicpMatrixCoefficients::ChromaticityDerivedConstant,
            14 => CicpMatrixCoefficients::Bt2100,
            15 => CicpMatrixCoefficients::IptPqC2,
            16 => CicpMatrixCoefficients::YCgCoRe,
            17 => CicpMatrixCoefficients::YCgCoRo,
            _ => return None,
        },
        full_range: match full_range {
            0 => CicpVideoFullRangeFlag::NarrowRange,
            1 => CicpVideoFullRangeFlag::FullRange,
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use image::{ImageBuffer, Rgb, Rgba};

    use super::*;

    #[test]
    fn safe_png_passes_through_without_recompression() {
        let bytes = png_bytes(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            4,
            3,
            Rgb([10, 20, 30]),
        )));

        let processed = preprocess_bytes(bytes.clone()).unwrap();

        assert_eq!(processed.bytes, bytes);
        assert!(!processed.report.recompressed);
        assert!(processed.report.metadata_stripped);
        assert_eq!(processed.report.source_format, "png");
    }

    #[test]
    fn source_files_remain_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.png");
        let bytes = png_bytes(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            4,
            3,
            Rgb([10, 20, 30]),
        )));
        fs::write(&path, &bytes).unwrap();
        let permissions = fs::metadata(&path).unwrap().permissions();

        preprocess_path(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(&path).unwrap().permissions(), permissions);
    }

    #[test]
    fn jpeg_orientation_is_applied_before_gps_metadata_is_removed() {
        let jpeg = jpeg_bytes(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            2,
            3,
            Rgb([20, 40, 60]),
        )));
        let source = insert_jpeg_segment(&jpeg, 0xe1, b"http://ns.adobe.com/xap/1.0/\0private-xmp");
        let source = insert_jpeg_segment(&source, 0xe2, b"private-color-profile");
        let source = insert_jpeg_segment(&source, 0xe1, &exif_with_orientation_and_gps(6));

        let processed = preprocess_bytes(source).unwrap();

        assert_eq!((processed.width, processed.height), (3, 2));
        assert_eq!(processed.report.uploaded_format, "jpeg");
        assert!(processed.report.recompressed);
        for private_value in [
            b"Exif".as_slice(),
            b"GPS".as_slice(),
            b"iPhone-device-id".as_slice(),
            b"embedded-thumbnail".as_slice(),
            b"private-xmp".as_slice(),
            b"private-color-profile".as_slice(),
        ] {
            assert!(
                !processed
                    .bytes
                    .windows(private_value.len())
                    .any(|value| value == private_value)
            );
        }
        assert!(jpeg_has_only_safe_segments(&processed.bytes));
    }

    #[test]
    fn malformed_images_are_rejected() {
        let error = preprocess_bytes(b"not an image".to_vec()).unwrap_err();
        assert_eq!(error.code, "draft.invalid_image");
    }

    #[test]
    fn decompression_bomb_dimensions_are_rejected_before_pixel_allocation() {
        let mut png = png_bytes(DynamicImage::new_rgb8(1, 1));
        png[16..20].copy_from_slice(&8_000_u32.to_be_bytes());
        png[20..24].copy_from_slice(&8_000_u32.to_be_bytes());
        update_png_chunk_crc(&mut png, 8);

        let error = preprocess_bytes(png).unwrap_err();

        assert_eq!(error.code, "draft.image_dimensions_exceeded");
    }

    #[test]
    fn cicp_color_is_converted_before_profile_metadata_is_removed() {
        let original = [190, 100, 40];
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(1, 1, Rgb(original)));
        let source = insert_png_chunk(&png_bytes(image), b"cICP", &[12, 13, 0, 1]);

        let processed = preprocess_bytes(source).unwrap();
        let converted = image::load_from_memory_with_format(&processed.bytes, ImageFormat::Png)
            .unwrap()
            .to_rgb8();

        assert_ne!(converted.get_pixel(0, 0).0, original);
        assert!(!processed.bytes.windows(4).any(|value| value == b"cICP"));
        assert!(processed.report.metadata_stripped);
    }

    #[test]
    fn alpha_is_preserved_when_metadata_requires_reencoding() {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_fn(3, 2, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 0])
            } else {
                Rgba([0, 255, 0, 255])
            }
        }));
        let source = insert_png_chunk(&png_bytes(image), b"tEXt", b"private-value");

        let processed = preprocess_bytes(source).unwrap();
        let decoded = image::load_from_memory_with_format(&processed.bytes, ImageFormat::Png)
            .unwrap()
            .to_rgba8();

        assert_eq!(processed.report.uploaded_format, "png");
        assert_eq!(decoded.get_pixel(0, 0).0[3], 0);
        assert!(
            !processed
                .bytes
                .windows(b"private-value".len())
                .any(|value| value == b"private-value")
        );
    }

    #[test]
    fn oversized_source_files_are_rejected() {
        let error =
            preprocess_bytes(vec![0; usize::try_from(MAX_SOURCE_BYTES + 1).unwrap()]).unwrap_err();
        assert_eq!(error.code, "draft.image_source_too_large");
    }

    #[test]
    fn output_bounds_and_encoding_are_deterministic() {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(3_000, 1_200, |x, y| {
            let value = x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17));
            Rgb([
                value as u8,
                value.wrapping_mul(3) as u8,
                value.wrapping_mul(7) as u8,
            ])
        }));
        let source = png_bytes(image);

        let first = preprocess_bytes(source.clone()).unwrap();
        let second = preprocess_bytes(source).unwrap();

        assert_eq!(first.bytes, second.bytes);
        assert_eq!(first.report, second.report);
        assert!(first.width <= MAX_FINAL_DIMENSION);
        assert!(first.height <= MAX_FINAL_DIMENSION);
        assert!(first.bytes.len() <= MAX_FINAL_BYTES);
    }

    #[test]
    fn heif_container_is_recognized() {
        assert_eq!(
            detect_source_format(&minimal_heif()).unwrap(),
            SourceFormat::Heif
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_heic_orientation_is_normalized() {
        let source = include_bytes!("../tests/fixtures/heic-orientation-6.heic").to_vec();

        let processed = preprocess_bytes(source).unwrap();

        assert_eq!(processed.report.source_format, "heic");
        assert_eq!((processed.width, processed.height), (3, 2));
        assert!(processed.report.metadata_stripped);
    }

    #[cfg(unix)]
    #[test]
    fn heic_orientation_is_normalized_and_temporary_files_are_cleaned() {
        let root = tempfile::tempdir().unwrap();
        let program_dir = tempfile::tempdir().unwrap();
        let decoded = png_bytes(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            2,
            3,
            Rgb([80, 90, 100]),
        )));
        let exif = exif_with_orientation_and_gps(6);
        let decoded = insert_png_chunk(&decoded, b"eXIf", &exif[6..]);
        fs::write(program_dir.path().join("decoded.png"), decoded).unwrap();
        let program = fake_decoder(program_dir.path(), true);
        let programs = [DecoderProgram {
            executable: program,
            kind: DecoderKind::Libheif,
        }];
        let settings = DecoderSettings {
            programs: &programs,
            temp_root: Some(root.path()),
            timeout: Duration::from_secs(2),
        };

        let source = include_bytes!("../tests/fixtures/heic-orientation-6.heic").to_vec();
        let processed = preprocess_inner(source, Some(&settings), &test_context()).unwrap();

        assert_eq!(processed.report.source_format, "heic");
        assert_eq!((processed.width, processed.height), (3, 2));
        assert!(processed.report.metadata_stripped);
        assert!(!processed.bytes.windows(3).any(|value| value == b"GPS"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn large_heic_is_resized_by_sips_during_decode() {
        let root = tempfile::tempdir().unwrap();
        let program_dir = tempfile::tempdir().unwrap();
        let decoded = png_bytes(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            1_600,
            1_200,
            Rgb([80, 90, 100]),
        )));
        fs::write(program_dir.path().join("decoded.png"), decoded).unwrap();
        let program = fake_sips(program_dir.path());
        let programs = [DecoderProgram {
            executable: program,
            kind: DecoderKind::Sips,
        }];
        let settings = DecoderSettings {
            programs: &programs,
            temp_root: Some(root.path()),
            timeout: Duration::from_secs(2),
        };

        let processed = preprocess_inner(minimal_heic(), Some(&settings), &test_context()).unwrap();

        assert_eq!((processed.width, processed.height), (1_600, 1_200));
        assert_eq!(processed.report.uploaded_format, "jpeg");
        assert!(processed.report.metadata_stripped);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn heic_decoder_failure_cleans_private_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let program_dir = tempfile::tempdir().unwrap();
        let program = fake_decoder(program_dir.path(), false);
        let programs = [DecoderProgram {
            executable: program,
            kind: DecoderKind::Libheif,
        }];
        let settings = DecoderSettings {
            programs: &programs,
            temp_root: Some(root.path()),
            timeout: Duration::from_secs(2),
        };

        let error = preprocess_inner(minimal_heic(), Some(&settings), &test_context()).unwrap_err();

        assert_eq!(error.code, "draft.heic_decode_failed");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn heic_decoder_timeout_is_bounded_and_cleans_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let program_dir = tempfile::tempdir().unwrap();
        let program = sleeping_decoder(program_dir.path());
        let programs = [DecoderProgram {
            executable: program,
            kind: DecoderKind::Libheif,
        }];
        let settings = DecoderSettings {
            programs: &programs,
            temp_root: Some(root.path()),
            timeout: Duration::from_millis(100),
        };
        let started = Instant::now();

        let error = preprocess_inner(minimal_heic(), Some(&settings), &test_context()).unwrap_err();

        assert_eq!(error.code, "draft.heic_decode_timeout");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn total_timeout_reports_the_stage_and_stops_the_worker() {
        let worker_running = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&worker_running);
        let error = run_bounded(Duration::from_millis(20), move |context| {
            worker_state.store(true, Ordering::Release);
            loop {
                if let Err(error) = context.enter(ProcessingStage::ColorNormalization) {
                    worker_state.store(false, Ordering::Release);
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })
        .unwrap_err();

        assert_eq!(error.code, "draft.image_processing_timeout");
        assert_eq!(
            error.details.as_ref().unwrap()["stage"],
            "color_normalization"
        );
        let stopped_by = Instant::now() + Duration::from_secs(1);
        while worker_running.load(Ordering::Acquire) && Instant::now() < stopped_by {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!worker_running.load(Ordering::Acquire));
    }

    #[test]
    fn missing_heic_decoder_returns_an_actionable_error() {
        let root = tempfile::tempdir().unwrap();
        let programs = [DecoderProgram {
            executable: root.path().join("missing-heif-decoder"),
            kind: DecoderKind::Libheif,
        }];
        let settings = DecoderSettings {
            programs: &programs,
            temp_root: Some(root.path()),
            timeout: Duration::from_secs(1),
        };

        let error = preprocess_inner(minimal_heic(), Some(&settings), &test_context()).unwrap_err();

        assert_eq!(error.code, "draft.heic_decoder_unavailable");
        assert!(error.message.contains("install libheif"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn temporary_workspace_permissions_are_private() {
        let root = tempfile::tempdir().unwrap();
        let workspace = TempWorkspace::new(Some(root.path())).unwrap();

        assert_eq!(
            fs::metadata(workspace.directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&workspace.input).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&workspace.output)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    fn test_context() -> ProcessingContext {
        ProcessingContext {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: Arc::new(AtomicBool::new(false)),
            stage: Arc::new(AtomicU8::new(ProcessingStage::Starting as u8)),
        }
    }

    fn png_bytes(image: DynamicImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn jpeg_bytes(image: DynamicImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
        bytes.into_inner()
    }

    fn insert_jpeg_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        bytes.extend_from_slice(&jpeg[..2]);
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&jpeg[2..]);
        bytes
    }

    fn exif_with_orientation_and_gps(orientation: u16) -> Vec<u8> {
        let mut payload = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        payload.extend_from_slice(&2_u16.to_le_bytes());
        payload.extend_from_slice(&0x0112_u16.to_le_bytes());
        payload.extend_from_slice(&3_u16.to_le_bytes());
        payload.extend_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&orientation.to_le_bytes());
        payload.extend_from_slice(&0_u16.to_le_bytes());
        payload.extend_from_slice(&0x8825_u16.to_le_bytes());
        payload.extend_from_slice(&4_u16.to_le_bytes());
        payload.extend_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&38_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&0_u16.to_le_bytes());
        payload.extend_from_slice(b"GPS iPhone-device-id embedded-thumbnail");
        payload
    }

    fn insert_png_chunk(png: &[u8], kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let idat = png.windows(4).position(|window| window == b"IDAT").unwrap() - 4;
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(payload);
        chunk.extend_from_slice(&png_crc(kind.iter().chain(payload).copied()).to_be_bytes());
        let mut output = Vec::with_capacity(png.len() + chunk.len());
        output.extend_from_slice(&png[..idat]);
        output.extend_from_slice(&chunk);
        output.extend_from_slice(&png[idat..]);
        output
    }

    fn update_png_chunk_crc(png: &mut [u8], offset: usize) {
        let length = usize::try_from(u32::from_be_bytes(
            png[offset..offset + 4].try_into().unwrap(),
        ))
        .unwrap();
        let crc_offset = offset + 8 + length;
        let crc = png_crc(png[offset + 4..crc_offset].iter().copied());
        png[crc_offset..crc_offset + 4].copy_from_slice(&crc.to_be_bytes());
    }

    fn png_crc(bytes: impl Iterator<Item = u8>) -> u32 {
        let mut crc = 0xffff_ffff_u32;
        for byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320_u32 & (0_u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    fn minimal_heic() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"heic");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"mif1");
        bytes.extend_from_slice(b"heic");
        bytes
    }

    fn minimal_heif() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"mif1");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"mif1");
        bytes
    }

    #[cfg(unix)]
    fn fake_decoder(directory: &Path, succeeds: bool) -> PathBuf {
        let body = if succeeds {
            "#!/usr/bin/env bash\nset -euo pipefail\nscript_dir=$(cd -- \"$(dirname -- \"$0\")\" && pwd)\ncp -- \"$script_dir/decoded.png\" \"${@: -1}\"\n"
        } else {
            "#!/usr/bin/env bash\nset -euo pipefail\nexit 1\n"
        };
        executable_script(directory, "fake-heif-decoder", body)
    }

    #[cfg(unix)]
    fn fake_sips(directory: &Path) -> PathBuf {
        executable_script(
            directory,
            "fake-sips",
            "#!/usr/bin/env bash\nset -euo pipefail\nscript_dir=$(cd -- \"$(dirname -- \"$0\")\" && pwd)\nif [[ \" $* \" == *\" -g pixelWidth \"* ]]; then\n  printf '  pixelWidth: 5712\\n  pixelHeight: 4284\\n'\n  exit 0\nfi\n[[ \" $* \" == *\" --resampleHeightWidthMax 2560 \"* ]]\ncp -- \"$script_dir/decoded.png\" \"${@: -1}\"\n",
        )
    }

    #[cfg(unix)]
    fn sleeping_decoder(directory: &Path) -> PathBuf {
        executable_script(
            directory,
            "sleeping-heif-decoder",
            "#!/usr/bin/env bash\nset -euo pipefail\nexec sleep 5\n",
        )
    }

    #[cfg(unix)]
    fn executable_script(directory: &Path, name: &str, body: &str) -> PathBuf {
        let program = directory.join(name);
        fs::write(&program, body).unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).unwrap();
        program
    }
}
