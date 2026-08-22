use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Number, Value};

use super::field::{Field, FieldOption, FieldStatus, FieldType, ValidationIssue, options_by_field};

macro_rules! string_enum_with_unknown {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum $name {
            $($variant,)+
            Unknown(String),
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = match self {
                    $(Self::$variant => $value,)+
                    Self::Unknown(value) => value,
                };
                serializer.serialize_str(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($value => Self::$variant,)+
                    _ => Self::Unknown(value),
                })
            }
        }
    };
}

string_enum_with_unknown!(DraftState {
    Draft => "draft",
    Pending => "pending",
    Active => "active",
    Rejected => "rejected",
    Expired => "expired",
    Deleted => "deleted",
});

string_enum_with_unknown!(ImageState {
    Processing => "processing",
    Ready => "ready",
    Failed => "failed",
});

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMethod {
    Pickup,
    Shipping,
    MeetUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Price {
    Number(Number),
    Text(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShippingInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryInput {
    #[serde(default)]
    pub methods: Vec<DeliveryMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping: Option<ShippingInput>,
}

/// Strict JSON input for create and update operations.
///
/// Unknown top-level and nested keys are rejected. Dynamic category data belongs
/// in `attributes`, whose values intentionally remain protocol-shaped JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trade_type: Option<TradeType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryInput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl DraftInput {
    pub fn from_json_slice(input: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(input)
    }

    pub fn present_keys(&self) -> BTreeSet<&'static str> {
        let mut keys = BTreeSet::new();
        if self.category.is_some() {
            keys.insert("category");
        }
        if self.title.is_some() {
            keys.insert("title");
        }
        if self.description.is_some() {
            keys.insert("description");
        }
        if self.price.is_some() {
            keys.insert("price");
        }
        if self.trade_type.is_some() {
            keys.insert("trade_type");
        }
        if self.postal_code.is_some() {
            keys.insert("postal_code");
        }
        if self.delivery.is_some() {
            keys.insert("delivery");
        }
        if !self.attributes.is_empty() {
            keys.insert("attributes");
        }
        if !self.images.is_empty() {
            keys.insert("images");
        }
        keys
    }

    pub fn reject_duplicate_sources<'a>(
        &self,
        flag_fields: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), DuplicateInputSources> {
        let json_fields = self.present_keys();
        let duplicates = flag_fields
            .into_iter()
            .filter(|field| json_fields.contains(*field))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if duplicates.is_empty() {
            Ok(())
        } else {
            Err(DuplicateInputSources { fields: duplicates })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateInputSources {
    pub fields: Vec<String>,
}

impl fmt::Display for DuplicateInputSources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fields supplied by both flags and JSON: {}",
            self.fields.join(", ")
        )
    }
}

impl std::error::Error for DuplicateInputSources {}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trade_type: Option<TradeType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryInput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
}

impl DraftValues {
    /// Applies only supplied values. Attribute entries are merged by key.
    pub fn merge(&mut self, input: DraftInput) {
        if input.category.is_some() {
            self.category = input.category;
        }
        if input.title.is_some() {
            self.title = input.title;
        }
        if input.description.is_some() {
            self.description = input.description;
        }
        if input.price.is_some() {
            self.price = input.price;
        }
        if input.trade_type.is_some() {
            self.trade_type = input.trade_type;
        }
        if input.postal_code.is_some() {
            self.postal_code = input.postal_code;
        }
        if input.delivery.is_some() {
            self.delivery = input.delivery;
        }
        self.attributes.extend(input.attributes);
    }

    /// Changes category and removes attributes outside the replacement schema.
    pub fn change_category(
        &mut self,
        category: impl Into<String>,
        schema: &CategorySchema,
    ) -> Vec<String> {
        self.category = Some(category.into());
        let fields = schema
            .fields
            .iter()
            .map(|field| (field.key.as_str(), &field.field_type))
            .collect::<BTreeMap<_, _>>();
        let options = options_by_field(&schema.options);
        let mut cleared = Vec::new();
        self.attributes.retain(|key, value| {
            let valid = fields.get(key.as_str()).is_some_and(|field_type| {
                if matches!(field_type, FieldType::Select | FieldType::MultiSelect) {
                    options.get(key.as_str()).is_none_or(|allowed| match value {
                        Value::Array(values) => values.iter().all(|value| allowed.contains(&value)),
                        value => allowed.contains(&&*value),
                    })
                } else {
                    true
                }
            });
            if !valid {
                cleared.push(key.clone());
            }
            valid
        });
        cleared.sort();
        cleared
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CategorySchema {
    pub fields: Vec<Field>,
    pub options: Vec<FieldOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftImage {
    pub image_id: String,
    pub position: usize,
    pub status: ImageState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoryPrediction {
    pub category: String,
    pub label: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftData {
    pub draft_id: String,
    pub state: DraftState,
    pub fields: Vec<Field>,
    pub options: Vec<FieldOption>,
    pub images: Vec<DraftImage>,
    pub category_predictions: Vec<CategoryPrediction>,
    pub validation: Vec<ValidationIssue>,
    pub cleared_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

impl DraftData {
    pub fn apply_validation(&mut self) {
        for issue in &self.validation {
            if let Some(field) = self
                .fields
                .iter_mut()
                .find(|field| field.key == issue.field)
            {
                field.status = FieldStatus::Invalid;
                field.validation_message = Some(issue.message.clone());
            }
        }
    }
}

pub type DraftCreateData = DraftData;
pub type DraftShowData = DraftData;
pub type DraftUpdateData = DraftData;
pub type CreateDraftResponse = DraftData;
pub type ShowDraftResponse = DraftData;
pub type UpdateDraftResponse = DraftData;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftRef {
    pub draft_id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::field::Requirement;

    fn attribute_field(key: &str, field_type: FieldType) -> Field {
        Field::new(
            key,
            key,
            field_type,
            Requirement::Unknown,
            None,
            "attributes",
        )
    }

    #[test]
    fn strict_input_rejects_unknown_keys_and_duplicate_sources() {
        let error = DraftInput::from_json_slice(br#"{"titel":"Chair"}"#).unwrap_err();
        assert!(error.to_string().contains("unknown field `titel`"));

        let input =
            DraftInput::from_json_slice(br#"{"title":"Chair","attributes":{"material":"10"}}"#)
                .unwrap();
        let error = input
            .reject_duplicate_sources(["category", "title", "title"])
            .unwrap_err();
        assert_eq!(error.fields, ["title"]);
    }

    #[test]
    fn partial_merge_preserves_absent_values_and_merges_attributes() {
        let mut values = DraftValues {
            title: Some("Old title".into()),
            description: Some("Keep this".into()),
            attributes: BTreeMap::from([("material".into(), json!("10"))]),
            ..DraftValues::default()
        };
        values.merge(DraftInput {
            title: Some("New title".into()),
            attributes: BTreeMap::from([("color".into(), json!("blue"))]),
            ..DraftInput::default()
        });

        assert_eq!(values.title.as_deref(), Some("New title"));
        assert_eq!(values.description.as_deref(), Some("Keep this"));
        assert_eq!(values.attributes["material"], "10");
        assert_eq!(values.attributes["color"], "blue");
    }

    #[test]
    fn category_change_clears_absent_and_invalid_attributes() {
        let mut values = DraftValues {
            attributes: BTreeMap::from([
                ("material".into(), json!("10")),
                ("color".into(), json!("red")),
                ("obsolete".into(), json!(true)),
            ]),
            ..DraftValues::default()
        };
        let schema = CategorySchema {
            fields: vec![
                attribute_field("material", FieldType::Select),
                attribute_field("color", FieldType::String),
            ],
            options: vec![FieldOption {
                field: "material".into(),
                value: json!("11"),
                label: "Metal".into(),
                raw: None,
            }],
            raw: Some(json!({"upstream": true})),
        };

        let cleared = values.change_category("furniture", &schema);

        assert_eq!(cleared, ["material", "obsolete"]);
        assert_eq!(
            values.attributes,
            BTreeMap::from([("color".into(), json!("red"))])
        );
    }

    #[test]
    fn response_serializes_flat_fields_options_and_unknown_states() {
        let response = DraftData {
            draft_id: "36443414".into(),
            state: DraftState::Unknown("moderating".into()),
            fields: vec![attribute_field("material", FieldType::Select)],
            options: vec![FieldOption {
                field: "material".into(),
                value: json!(10),
                label: "Wood".into(),
                raw: None,
            }],
            images: vec![DraftImage {
                image_id: "image-1".into(),
                position: 0,
                status: ImageState::Processing,
                width: None,
                height: None,
                error: None,
                raw: None,
            }],
            category_predictions: vec![CategoryPrediction {
                category: "furniture".into(),
                label: "Furniture".into(),
                confidence: 0.9,
                raw: None,
            }],
            validation: vec![],
            cleared_fields: vec![],
            raw: None,
        };
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["state"], "moderating");
        assert_eq!(value["fields"][0]["key"], "material");
        assert_eq!(value["options"][0]["value"], 10);
        assert_eq!(value["images"][0]["status"], "processing");
    }

    #[test]
    fn validation_marks_normalized_field_invalid() {
        let mut response = DraftData {
            draft_id: "draft-1".into(),
            state: DraftState::Draft,
            fields: vec![Field::new(
                "title",
                "Title",
                FieldType::String,
                Requirement::Required,
                Some(json!("x")),
                "details",
            )],
            options: vec![],
            images: vec![],
            category_predictions: vec![],
            validation: vec![ValidationIssue {
                field: "title".into(),
                code: "too_short".into(),
                message: "Title is too short".into(),
                source: None,
                raw: None,
            }],
            cleared_fields: vec![],
            raw: None,
        };

        response.apply_validation();

        assert_eq!(response.fields[0].status, FieldStatus::Invalid);
        assert_eq!(
            response.fields[0].validation_message.as_deref(),
            Some("Title is too short")
        );
    }
}
