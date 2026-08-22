use std::error::Error;

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
    pub details: Option<Box<Value>>,
    pub partial: Option<Box<Value>>,
    pub next_actions: Vec<NextAction>,
    pub diagnostics: Option<Box<Diagnostics>>,
    pub exit_class: ExitClass,
    #[source]
    source_error: Option<Box<dyn Error + Send + Sync>>,
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("cli.invalid_usage", message, ExitClass::Usage)
    }

    pub fn authentication(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, ExitClass::Authentication)
    }

    pub fn validation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, ExitClass::Validation)
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, ExitClass::Conflict)
    }

    pub fn upstream(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, ExitClass::Upstream)
    }

    pub fn partial(code: impl Into<String>, message: impl Into<String>, partial: Value) -> Self {
        let mut error = Self::new(code, message, ExitClass::Partial);
        error.partial = Some(Box::new(partial));
        error
    }

    pub fn output(message: impl Into<String>) -> Self {
        Self::new("output.serialization_failed", message, ExitClass::Upstream)
    }

    pub fn unexpected(message: impl Into<String>) -> Self {
        Self::new("internal.unexpected", message, ExitClass::Upstream)
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
            source_error: None,
        }
    }

    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source_error = Some(Box::new(source));
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    pub fn with_partial(mut self, partial: Value) -> Self {
        self.partial = Some(Box::new(partial));
        self.exit_class = ExitClass::Partial;
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn internal_chain(&self) -> Vec<String> {
        let mut chain = vec![self.to_string()];
        let mut source = self.source();
        while let Some(error) = source {
            chain.push(error.to_string());
            source = error.source();
        }
        chain
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
            details: error.details.as_deref().cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fmt};

    use super::*;

    #[derive(Debug)]
    struct Outer(Inner);

    impl fmt::Display for Outer {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("outer")
        }
    }

    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[derive(Debug)]
    struct Inner;

    impl fmt::Display for Inner {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("inner")
        }
    }

    impl Error for Inner {}

    #[test]
    fn preserves_the_complete_internal_error_chain() {
        let error =
            AppError::upstream("upstream.failed", "request failed").with_source(Outer(Inner));
        assert_eq!(error.internal_chain(), ["request failed", "outer", "inner"]);
    }

    #[test]
    fn exit_classes_have_stable_codes() {
        assert_eq!(ExitClass::Success.code(), 0);
        assert_eq!(ExitClass::Usage.code(), 2);
        assert_eq!(ExitClass::Authentication.code(), 10);
        assert_eq!(ExitClass::Validation.code(), 20);
        assert_eq!(ExitClass::Conflict.code(), 30);
        assert_eq!(ExitClass::Upstream.code(), 40);
        assert_eq!(ExitClass::Partial.code(), 50);
    }
}
