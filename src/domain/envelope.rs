use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::observation::Observation,
    error::{AppError, ErrorBody},
};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Envelope<T = Value> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<NextAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Diagnostics>,
}

impl<T> Envelope<T> {
    pub fn success(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
            partial: None,
            observation: None,
            warnings: Vec::new(),
            next_actions: Vec::new(),
            diagnostics: None,
        }
    }
}

impl Envelope<Value> {
    pub fn failure(error: AppError) -> Self {
        let body = ErrorBody::from(&error);
        Self {
            ok: false,
            data: None,
            error: Some(body),
            partial: error.partial.map(|partial| *partial),
            observation: None,
            warnings: Vec::new(),
            next_actions: error.next_actions,
            diagnostics: error.diagnostics.map(|diagnostics| *diagnostics),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct NextAction {
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Diagnostics {
    pub trace_id: String,
    pub correlation_id: String,
    pub log_path: String,
}
