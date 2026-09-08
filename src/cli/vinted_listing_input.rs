use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use crate::{
    domain::vinted_listing_changes::{MAX_VINTED_LISTING_CHANGES_BYTES, VintedListingChanges},
    error::AppError,
};

pub(crate) fn read_vinted_listing_changes(path: &Path) -> Result<VintedListingChanges, AppError> {
    let bytes = if path == Path::new("-") {
        read_bounded(io::stdin().lock()).map_err(|error| {
            AppError::usage(format!(
                "Failed to read Vinted listing changes from stdin: {error}"
            ))
        })?
    } else {
        let file = File::open(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read Vinted listing changes input `{}`: {error}",
                path.display()
            ))
        })?;
        read_bounded(file).map_err(|error| {
            AppError::usage(format!(
                "Failed to read Vinted listing changes input `{}`: {error}",
                path.display()
            ))
        })?
    };
    VintedListingChanges::from_json_slice(&bytes)
}

fn read_bounded(reader: impl Read) -> Result<Vec<u8>, io::Error> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_VINTED_LISTING_CHANGES_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_partial_changes_from_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("changes.json");
        std::fs::write(&path, br#"{"price":"8.00"}"#).unwrap();

        let changes = read_vinted_listing_changes(&path).unwrap();

        assert_eq!(changes.title, None);
        assert_eq!(changes.description, None);
        assert_eq!(changes.price.as_deref(), Some("8.00"));
    }

    #[test]
    fn reports_file_read_failures_without_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.json");

        let error = read_vinted_listing_changes(&path).unwrap_err();

        assert_eq!(error.code, "cli.invalid_usage");
        assert!(
            error
                .message
                .contains("Failed to read Vinted listing changes input")
        );
    }

    #[test]
    fn bounds_file_input_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("changes.json");
        std::fs::write(&path, vec![b' '; MAX_VINTED_LISTING_CHANGES_BYTES + 1]).unwrap();

        let error = read_vinted_listing_changes(&path).unwrap_err();

        assert_eq!(error.code, "cli.invalid_usage");
        assert!(error.message.contains("exceeds 1 MiB"));
    }

    #[test]
    fn bounded_reader_never_consumes_more_than_the_limit_sentinel() {
        let input = vec![b'x'; MAX_VINTED_LISTING_CHANGES_BYTES + 100];

        let bytes = read_bounded(input.as_slice()).unwrap();

        assert_eq!(bytes.len(), MAX_VINTED_LISTING_CHANGES_BYTES + 1);
    }
}
