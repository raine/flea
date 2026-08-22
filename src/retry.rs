use reqwest::Method;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetryClassification {
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    Transport,
    HttpStatus(u16),
    MalformedSuccess,
    Conflict,
    PreconditionFailed,
    Local,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

impl OperationMethod {
    pub fn from_reqwest(method: &Method) -> Self {
        match *method {
            Method::GET => Self::Get,
            Method::HEAD => Self::Head,
            Method::POST => Self::Post,
            Method::PUT => Self::Put,
            Method::PATCH => Self::Patch,
            Method::DELETE => Self::Delete,
            _ => Self::Post,
        }
    }

    pub const fn is_read(self) -> bool {
        matches!(self, Self::Get | Self::Head)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryContext {
    pub method: OperationMethod,
    pub idempotency_contract: bool,
    pub idempotency_key: bool,
    pub completed_mutation_steps: bool,
    pub returned_identifier: bool,
    pub etag: bool,
    pub authoritative_observation: bool,
}

impl RetryContext {
    pub const fn read(method: OperationMethod) -> Self {
        Self {
            method,
            idempotency_contract: false,
            idempotency_key: false,
            completed_mutation_steps: false,
            returned_identifier: false,
            etag: false,
            authoritative_observation: false,
        }
    }

    pub const fn mutation(method: OperationMethod) -> Self {
        Self {
            method,
            idempotency_contract: false,
            idempotency_key: false,
            completed_mutation_steps: false,
            returned_identifier: false,
            etag: false,
            authoritative_observation: false,
        }
    }

    pub const fn with_idempotency_contract(mut self) -> Self {
        self.idempotency_contract = true;
        self
    }

    pub const fn with_idempotency_key(mut self) -> Self {
        self.idempotency_key = true;
        self
    }

    pub const fn with_completed_mutation_steps(mut self) -> Self {
        self.completed_mutation_steps = true;
        self
    }

    pub const fn with_returned_identifier(mut self) -> Self {
        self.returned_identifier = true;
        self
    }

    pub const fn with_etag(mut self) -> Self {
        self.etag = true;
        self
    }

    pub const fn with_authoritative_observation(mut self) -> Self {
        self.authoritative_observation = true;
        self
    }
}

pub const fn classify(failure: FailureKind, context: RetryContext) -> RetryClassification {
    let upstream_transient = match failure {
        FailureKind::Transport => true,
        FailureKind::HttpStatus(status) => is_transient_status(status),
        FailureKind::MalformedSuccess
        | FailureKind::Conflict
        | FailureKind::PreconditionFailed
        | FailureKind::Local => false,
    };

    let safe_to_retry = if context.completed_mutation_steps || context.returned_identifier {
        false
    } else if context.method.is_read() || context.idempotency_contract || context.idempotency_key {
        true
    } else {
        matches!(failure, FailureKind::PreconditionFailed)
            && context.etag
            && context.authoritative_observation
    };

    RetryClassification {
        upstream_transient,
        safe_to_retry,
    }
}

pub const fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_failures_are_safe_independently_of_transience() {
        for (failure, transient) in [
            (FailureKind::Transport, true),
            (FailureKind::HttpStatus(429), true),
            (FailureKind::HttpStatus(502), true),
            (FailureKind::MalformedSuccess, false),
            (FailureKind::HttpStatus(404), false),
        ] {
            assert_eq!(
                classify(failure, RetryContext::read(OperationMethod::Get)),
                RetryClassification {
                    upstream_transient: transient,
                    safe_to_retry: true,
                }
            );
        }
    }

    #[test]
    fn uncertain_mutations_preserve_transience_without_allowing_replay() {
        for failure in [
            FailureKind::Transport,
            FailureKind::HttpStatus(429),
            FailureKind::HttpStatus(500),
            FailureKind::HttpStatus(502),
            FailureKind::HttpStatus(503),
        ] {
            assert_eq!(
                classify(failure, RetryContext::mutation(OperationMethod::Put)),
                RetryClassification {
                    upstream_transient: true,
                    safe_to_retry: false,
                }
            );
        }
        assert_eq!(
            classify(
                FailureKind::MalformedSuccess,
                RetryContext::mutation(OperationMethod::Post)
            ),
            RetryClassification::default()
        );
    }

    #[test]
    fn source_backed_idempotency_allows_mutation_retries() {
        let contract = RetryContext::mutation(OperationMethod::Put).with_idempotency_contract();
        let key = RetryContext::mutation(OperationMethod::Post).with_idempotency_key();

        assert!(classify(FailureKind::Transport, contract).safe_to_retry);
        assert!(classify(FailureKind::HttpStatus(503), key).safe_to_retry);
    }

    #[test]
    fn partial_workflows_and_returned_ids_prevent_whole_command_replay() {
        let completed = RetryContext::read(OperationMethod::Get).with_completed_mutation_steps();
        let identified = RetryContext::mutation(OperationMethod::Post)
            .with_idempotency_key()
            .with_returned_identifier();

        assert!(!classify(FailureKind::Transport, completed).safe_to_retry);
        assert!(!classify(FailureKind::Transport, identified).safe_to_retry);
    }

    #[test]
    fn observed_etag_precondition_failure_is_safe_but_not_transient() {
        let context = RetryContext::mutation(OperationMethod::Put)
            .with_etag()
            .with_authoritative_observation();
        assert_eq!(
            classify(FailureKind::PreconditionFailed, context),
            RetryClassification {
                upstream_transient: false,
                safe_to_retry: true,
            }
        );
        assert_eq!(
            classify(FailureKind::Conflict, context),
            RetryClassification::default()
        );
    }
}
