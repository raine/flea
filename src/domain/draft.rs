use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

pub use super::commerce::TradeType;
use super::field::{Field, FieldOption, FieldType, options_by_field};

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::field::{FieldType, Requirement};

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
}
