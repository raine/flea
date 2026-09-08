use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::error::AppError;

pub const MAX_VINTED_LISTING_CHANGES_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedListingChanges {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
}

impl VintedListingChanges {
    pub fn new(
        title: Option<String>,
        description: Option<String>,
        price: Option<String>,
    ) -> Result<Self, AppError> {
        let changes = Self {
            title,
            description,
            price,
        };
        changes.validate()?;
        Ok(changes)
    }

    pub fn from_json_slice(input: &[u8]) -> Result<Self, AppError> {
        if input.len() > MAX_VINTED_LISTING_CHANGES_BYTES {
            return Err(AppError::usage(
                "Vinted listing changes input exceeds 1 MiB",
            ));
        }
        let raw: RawChanges = serde_json::from_slice(input).map_err(|error| {
            AppError::usage(format!(
                "Vinted listing changes must be a JSON object containing only title, description, or price string fields at line {} column {}",
                error.line(),
                error.column()
            ))
        })?;
        Self::new(raw.title, raw.description, raw.price)
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.title.is_none() && self.description.is_none() && self.price.is_none() {
            return Err(AppError::usage(
                "Vinted listing update requires at least one changed field",
            ));
        }
        for (name, value) in [
            ("title", self.title.as_deref()),
            ("description", self.description.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(AppError::validation(
                    "vinted.required_field",
                    format!("{name} must not be empty"),
                ));
            }
        }
        if let Some(price) = self.price.as_deref() {
            let parsed = price.parse::<f64>().ok();
            if parsed.is_none_or(|price| !price.is_finite() || price <= 0.0) {
                return Err(AppError::validation(
                    "vinted.invalid_price",
                    "Price must be a decimal string greater than zero",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct RawChanges {
    title: Option<String>,
    description: Option<String>,
    price: Option<String>,
}

impl<'de> Deserialize<'de> for RawChanges {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawChangesVisitor)
    }
}

struct RawChangesVisitor;

impl<'de> de::Visitor<'de> for RawChangesVisitor {
    type Value = RawChanges;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object containing title, description, or price string fields")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut changes = RawChanges::default();
        let mut title_seen = false;
        let mut description_seen = false;
        let mut price_seen = false;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "title" => {
                    if title_seen {
                        return Err(de::Error::duplicate_field("title"));
                    }
                    title_seen = true;
                    changes.title = Some(map.next_value()?);
                }
                "description" => {
                    if description_seen {
                        return Err(de::Error::duplicate_field("description"));
                    }
                    description_seen = true;
                    changes.description = Some(map.next_value()?);
                }
                "price" => {
                    if price_seen {
                        return Err(de::Error::duplicate_field("price"));
                    }
                    price_seen = true;
                    changes.price = Some(map.next_value()?);
                }
                _ => return Err(de::Error::unknown_field(&key, FIELDS)),
            }
        }

        Ok(changes)
    }
}

const FIELDS: &[&str] = &["title", "description", "price"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExitClass;

    #[test]
    fn omitted_fields_remain_absent() {
        let changes = VintedListingChanges::from_json_slice(br#"{"title":"New title"}"#).unwrap();

        assert_eq!(changes.title.as_deref(), Some("New title"));
        assert_eq!(changes.description, None);
        assert_eq!(changes.price, None);
        assert_eq!(
            serde_json::to_value(changes).unwrap(),
            serde_json::json!({"title": "New title"})
        );
    }

    #[test]
    fn accepts_all_supported_string_fields() {
        let changes = VintedListingChanges::from_json_slice(
            br#"{"title":"Title","description":"Description","price":"12.50"}"#,
        )
        .unwrap();

        assert_eq!(changes.description.as_deref(), Some("Description"));
        assert_eq!(changes.price.as_deref(), Some("12.50"));
    }

    #[test]
    fn rejects_invalid_json_shapes_and_fields_as_usage_errors() {
        for input in [
            br#"[]"#.as_slice(),
            br#"{}"#,
            br#"{"photos":[]}"#,
            br#"{"title":null}"#,
            br#"{"description":7}"#,
            br#"{"price":12.5}"#,
            br#"{"title":"one","title":"two"}"#,
            br#"{"title":"one"} trailing"#,
        ] {
            let error = VintedListingChanges::from_json_slice(input).unwrap_err();
            assert_eq!(error.exit_class, ExitClass::Usage, "input: {input:?}");
            assert_eq!(error.code, "cli.invalid_usage", "input: {input:?}");
        }
    }

    #[test]
    fn rejects_empty_text_and_invalid_prices_with_publication_error_codes() {
        for (input, expected_code) in [
            (br#"{"title":"  "}"#.as_slice(), "vinted.required_field"),
            (br#"{"description":"\n"}"#, "vinted.required_field"),
            (br#"{"price":"0"}"#, "vinted.invalid_price"),
            (br#"{"price":"-1"}"#, "vinted.invalid_price"),
            (br#"{"price":"NaN"}"#, "vinted.invalid_price"),
            (br#"{"price":"not a price"}"#, "vinted.invalid_price"),
        ] {
            let error = VintedListingChanges::from_json_slice(input).unwrap_err();
            assert_eq!(error.exit_class, ExitClass::Validation, "input: {input:?}");
            assert_eq!(error.code, expected_code, "input: {input:?}");
        }
    }

    #[test]
    fn rejects_oversized_slices_before_parsing() {
        let input = vec![b' '; MAX_VINTED_LISTING_CHANGES_BYTES + 1];
        let error = VintedListingChanges::from_json_slice(&input).unwrap_err();

        assert_eq!(error.code, "cli.invalid_usage");
        assert!(error.message.contains("exceeds 1 MiB"));
    }

    #[test]
    fn invalid_input_errors_do_not_echo_json_values() {
        let secret = "private-secret-sentinel";
        let input = format!(r#""{secret}""#);

        let error = VintedListingChanges::from_json_slice(input.as_bytes()).unwrap_err();
        let diagnostic = format!("{error:?}");

        assert_eq!(error.code, "cli.invalid_usage");
        assert!(!error.message.contains(secret));
        assert!(!diagnostic.contains(secret));
    }
}
