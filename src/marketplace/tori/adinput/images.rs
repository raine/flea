use super::http::ApiError;
use super::types::{DraftImage, UploadedImage};
use crate::image_processing;
use crate::image_processing::ImageProcessingReport;
use crate::image_processing::ProcessedImage;
use crate::image_processing::ProcessingError;
use serde_json::Value;
use std::fmt;
use std::path::Path;

pub struct PreparedImage {
    pub(super) bytes: Vec<u8>,
    pub(super) file_name: &'static str,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) report: ImageProcessingReport,
}

impl PreparedImage {
    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub fn source_format(&self) -> &str {
        &self.report.source_format
    }

    pub fn output_format(&self) -> &str {
        &self.report.uploaded_format
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub const fn metadata_stripped(&self) -> bool {
        self.report.metadata_stripped
    }

    pub const fn recompressed(&self) -> bool {
        self.report.recompressed
    }

    pub(super) fn processing_report(&self) -> &ImageProcessingReport {
        &self.report
    }
}

impl From<ProcessedImage> for PreparedImage {
    fn from(image: ProcessedImage) -> Self {
        Self {
            bytes: image.bytes,
            file_name: image.file_name,
            width: image.width,
            height: image.height,
            report: image.report,
        }
    }
}

impl fmt::Debug for PreparedImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedImage")
            .field("byte_len", &self.bytes.len())
            .field("file_name", &self.file_name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("report", &self.report)
            .finish()
    }
}

pub fn normalize_category(category: Value) -> Value {
    match category {
        Value::String(id) => id
            .parse::<u64>()
            .map(Value::from)
            .unwrap_or(Value::String(id)),
        category => category,
    }
}

pub fn prepare_image(path: &Path) -> Result<PreparedImage, ApiError> {
    let metadata = path.metadata().map_err(|_| {
        ApiError::new(
            "draft.image_read_failed",
            "Image file does not exist or cannot be read",
        )
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(
            "draft.image_read_failed",
            "Image path must identify a regular file",
        ));
    }
    image_processing::preprocess_path(path)
        .map(PreparedImage::from)
        .map_err(image_processing_error)
}

pub(super) fn prepare_image_bytes(bytes: Vec<u8>) -> Result<PreparedImage, ApiError> {
    image_processing::preprocess_bytes(bytes)
        .map(PreparedImage::from)
        .map_err(image_processing_error)
}

fn image_processing_error(error: ProcessingError) -> ApiError {
    let mut api_error = ApiError::new(error.code, error.message);
    api_error.details = error.details.map(Box::new);
    api_error
}

pub(super) fn uploaded_from_draft_image(image: &DraftImage) -> UploadedImage {
    UploadedImage {
        image_id: image.image_id.clone(),
        state: image.state.clone(),
        url: image.url.clone().or_else(|| Some(image.image_id.clone())),
        width: image.width,
        height: image.height,
        mime_type: image.mime_type.clone(),
    }
}
