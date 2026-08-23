use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::field::{Field, FieldOption, FieldStatus, FieldType, Requirement, ValidationIssue};

/// Marketplace-neutral state for a form that can produce a publication payload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PublicationForm {
    pub fields: Vec<Field>,
    pub options: Vec<FieldOption>,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default)]
    pub issues: Vec<ValidationIssue>,
    pub ready: bool,
}

impl PublicationForm {
    pub fn validate(&mut self) {
        self.issues.clear();
        let option_fields = self
            .options
            .iter()
            .map(|option| option.field.as_str())
            .collect::<BTreeSet<_>>();

        for field in &mut self.fields {
            let value = self.values.get(&field.key).cloned();
            field.value = value.clone();
            field.status = if value.as_ref().is_none_or(value_is_missing) {
                FieldStatus::Missing
            } else {
                FieldStatus::Set
            };
            field.validation_message = None;

            if field.requirement == Requirement::Required && field.status == FieldStatus::Missing {
                self.issues.push(issue(
                    &field.key,
                    "required",
                    format!("{} is required", field.label),
                ));
                continue;
            }
            let Some(value) = value.filter(|value| !value_is_missing(value)) else {
                continue;
            };
            if !value_has_type(&value, &field.field_type) {
                field.invalidate(expected_type(&field.field_type));
                self.issues.push(issue(
                    &field.key,
                    "invalid_type",
                    format!(
                        "{} must be {}",
                        field.label,
                        expected_type(&field.field_type)
                    ),
                ));
                continue;
            }
            if matches!(field.field_type, FieldType::Select | FieldType::MultiSelect)
                && option_fields.contains(field.key.as_str())
                && !selection_is_allowed(&field.key, &value, &self.options)
            {
                field.invalidate("the selection is absent from the discovered option set");
                self.issues.push(issue(
                    &field.key,
                    "invalid_option",
                    format!("{} contains an unavailable selection", field.label),
                ));
                continue;
            }
            if field.field_type == FieldType::Decimal
                && let Some(message) = numeric_bounds_error(&value, field.raw.as_ref())
            {
                field.invalidate(&message);
                self.issues.push(issue(&field.key, "out_of_range", message));
            }
        }
        self.ready = self.issues.is_empty();
    }
}

fn issue(field: &str, code: &str, message: String) -> ValidationIssue {
    ValidationIssue {
        field: field.to_owned(),
        code: code.to_owned(),
        message,
        source: None,
        raw: None,
    }
}

fn value_is_missing(value: &Value) -> bool {
    value.is_null()
        || value.as_str().is_some_and(str::is_empty)
        || value.as_array().is_some_and(Vec::is_empty)
}

fn value_has_type(value: &Value, field_type: &FieldType) -> bool {
    match field_type {
        FieldType::String | FieldType::Text | FieldType::Date => value.is_string(),
        FieldType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldType::Decimal => {
            value.is_number() || value.as_str().is_some_and(|v| v.parse::<f64>().is_ok())
        }
        FieldType::Boolean => value.is_boolean(),
        FieldType::Select => !value.is_array() || value.is_object(),
        FieldType::MultiSelect => value.is_array(),
        FieldType::Unknown(_) => true,
    }
}

fn expected_type(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::String | FieldType::Text => "text",
        FieldType::Integer => "an integer",
        FieldType::Decimal => "a decimal number",
        FieldType::Boolean => "a boolean",
        FieldType::Select => "one discovered option",
        FieldType::MultiSelect => "an array of discovered options",
        FieldType::Date => "a date string",
        FieldType::Unknown(_) => "a supported value",
    }
}

fn numeric_bounds_error(value: &Value, metadata: Option<&Value>) -> Option<String> {
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))?;
    let metadata = metadata?;
    let bound = |key| {
        metadata
            .get(key)
            .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
    };
    if let Some(minimum) = bound("minimum")
        && number < minimum
    {
        return Some(format!("value must be at least {minimum}"));
    }
    if let Some(maximum) = bound("maximum")
        && number > maximum
    {
        return Some(format!("value must be at most {maximum}"));
    }
    None
}

fn selection_is_allowed(field: &str, value: &Value, options: &[FieldOption]) -> bool {
    let allowed = options
        .iter()
        .filter(|option| option.field == field)
        .map(|option| &option.value)
        .collect::<Vec<_>>();
    match value {
        Value::Array(values) => values.iter().all(|value| allowed.contains(&value)),
        value => allowed.contains(&value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validation_reports_missing_and_rejects_unknown_options() {
        let mut form = PublicationForm {
            fields: vec![
                Field::new(
                    "title",
                    "Title",
                    FieldType::String,
                    Requirement::Required,
                    None,
                    "details",
                ),
                Field::new(
                    "color",
                    "Color",
                    FieldType::MultiSelect,
                    Requirement::Optional,
                    None,
                    "details",
                ),
            ],
            options: vec![FieldOption {
                field: "color".into(),
                value: json!(1),
                label: "Black".into(),
                raw: None,
            }],
            values: Map::from_iter([("color".into(), json!([2]))]),
            ..Default::default()
        };
        form.validate();
        assert_eq!(
            form.issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<Vec<_>>(),
            ["required", "invalid_option"]
        );
        assert!(!form.ready);
    }

    #[test]
    fn validation_applies_discovered_numeric_bounds() {
        let mut price = Field::new(
            "price",
            "Price",
            FieldType::Decimal,
            Requirement::Required,
            None,
            "price",
        );
        price.raw = Some(json!({"minimum": "1.00", "maximum": "100.00"}));
        let mut form = PublicationForm {
            fields: vec![price],
            values: Map::from_iter([("price".into(), json!("0.50"))]),
            ..Default::default()
        };
        form.validate();
        assert_eq!(form.issues[0].code, "out_of_range");
    }
}
