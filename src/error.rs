use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::envelope::{Diagnostics, NextAction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitClass {
    Success,
    Usage,
    Authentication,
    Validation,
    Conflict,
    Upstream,
    Partial,
}

impl ExitClass {
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 2,
            Self::Authentication => 10,
            Self::Validation => 20,
            Self::Conflict => 30,
            Self::Upstream => 40,
            Self::Partial => 50,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
    pub partial: Option<Value>,
    pub next_actions: Vec<NextAction>,
    pub diagnostics: Option<Diagnostics>,
    pub exit_class: ExitClass,
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("cli.invalid_usage", message, ExitClass::Usage)
    }

    pub fn protocol_unavailable(command: &str, details: Value) -> Self {
        let mut error = Self::new(
            "upstream.not_implemented",
            format!("remote protocol support for `{command}` is unavailable"),
            ExitClass::Upstream,
        );
        error.details = Some(details);
        error
    }

    pub fn output(message: impl Into<String>) -> Self {
        Self::new("output.serialization_failed", message, ExitClass::Upstream)
    }

    pub fn new(code: impl Into<String>, message: impl Into<String>, exit_class: ExitClass) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: None,
            partial: None,
            next_actions: Vec::new(),
            diagnostics: None,
            exit_class,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl From<&AppError> for ErrorBody {
    fn from(error: &AppError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            retryable: error.retryable,
            details: error.details.clone(),
        }
    }
}
