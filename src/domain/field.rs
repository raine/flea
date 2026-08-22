use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Required,
    Optional,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldStatus {
    Set,
    Missing,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Text,
    Integer,
    Decimal,
    Boolean,
    Select,
    MultiSelect,
    Date,
    #[serde(untagged)]
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub requirement: Requirement,
    pub status: FieldStatus,
    pub value: Option<Value>,
    pub section: String,
    #[serde(default)]
    pub option_count: usize,
    #[serde(default)]
    pub options_returned: usize,
    #[serde(default)]
    pub options_truncated: bool,
    #[serde(default, skip_serializing)]
    pub validation_options: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl Field {
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        field_type: FieldType,
        requirement: Requirement,
        value: Option<Value>,
        section: impl Into<String>,
    ) -> Self {
        let status = value
            .as_ref()
            .map_or(FieldStatus::Missing, |value| match value {
                Value::Null => FieldStatus::Missing,
                Value::String(value) if value.is_empty() => FieldStatus::Missing,
                Value::Array(value) if value.is_empty() => FieldStatus::Missing,
                _ => FieldStatus::Set,
            });
        Self {
            key: key.into(),
            label: label.into(),
            field_type,
            requirement,
            status,
            value,
            section: section.into(),
            option_count: 0,
            options_returned: 0,
            options_truncated: false,
            validation_options: Vec::new(),
            validation_message: None,
            raw: None,
        }
    }

    pub fn invalidate(&mut self, message: impl Into<String>) {
        self.status = FieldStatus::Invalid;
        self.validation_message = Some(message.into());
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldOption {
    pub field: String,
    pub value: Value,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub field: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpstreamValidationError {
    pub source: String,
    pub code: String,
    pub message: String,
    pub raw: Option<Value>,
}

/// Maps protocol field names to stable keys and keeps the source for unmapped errors.
pub fn map_validation_errors(
    errors: impl IntoIterator<Item = UpstreamValidationError>,
    fields: &[Field],
) -> Vec<ValidationIssue> {
    let known: BTreeSet<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    errors
        .into_iter()
        .map(|error| {
            let candidate = stable_field_key(&error.source);
            let mapped = known.contains(candidate.as_str()).then_some(candidate);
            ValidationIssue {
                field: mapped.unwrap_or_else(|| error.source.clone()),
                code: error.code,
                message: error.message,
                source: Some(error.source),
                raw: error.raw,
            }
        })
        .collect()
}

pub(crate) fn stable_field_key(source: &str) -> String {
    let leaf = source
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(source)
        .trim_matches(['[', ']']);
    match leaf {
        "subject" | "heading" => "title".to_owned(),
        "body" | "text" => "description".to_owned(),
        "categoryId" | "category_id" => "category".to_owned(),
        "tradeType" | "trade_type" => "trade_type".to_owned(),
        "postalCode" | "postal_code" => "postal_code".to_owned(),
        value => camel_to_snake(value),
    }
}

fn camel_to_snake(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_uppercase() {
            if !output.is_empty() {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

pub fn options_by_field(options: &[FieldOption]) -> BTreeMap<&str, Vec<&Value>> {
    let mut grouped = BTreeMap::<&str, Vec<&Value>>::new();
    for option in options {
        grouped
            .entry(option.field.as_str())
            .or_default()
            .push(&option.value);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fields_have_explicit_requirement_and_status() {
        let missing = Field::new(
            "title",
            "Title",
            FieldType::String,
            Requirement::Required,
            None,
            "details",
        );
        let set = Field::new(
            "condition",
            "Condition",
            FieldType::Select,
            Requirement::Unknown,
            Some(json!(3)),
            "details",
        );

        assert_eq!(missing.status, FieldStatus::Missing);
        assert_eq!(set.status, FieldStatus::Set);
        assert_eq!(serde_json::to_value(&set).unwrap()["type"], "select");
    }

    #[test]
    fn validation_errors_map_known_protocol_names_and_preserve_raw_errors() {
        let fields = vec![Field::new(
            "postal_code",
            "Postal code",
            FieldType::String,
            Requirement::Required,
            None,
            "details",
        )];
        let issues = map_validation_errors(
            [UpstreamValidationError {
                source: "item.postalCode".into(),
                code: "invalid".into(),
                message: "Invalid postal code".into(),
                raw: Some(json!({"name": "postalCode"})),
            }],
            &fields,
        );

        assert_eq!(issues[0].field, "postal_code");
        assert_eq!(issues[0].source.as_deref(), Some("item.postalCode"));
        assert!(issues[0].raw.is_some());
    }
}
