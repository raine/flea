use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Required,
    Optional,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub requirement: Requirement,
    pub value: Option<Value>,
}
