use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::retry::RetryClassification;

/// Stable classification of an attempt to observe remote state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationState {
    ConfirmedPresent,
    ConfirmedAbsent,
    TemporarilyUnavailable,
    UnrecognizedResponse,
    ConflictingSources,
}

/// The operation whose retry safety is being classified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationOperation {
    Read,
    Mutation,
    PostMutationVerification,
}

/// Bounded evidence that is safe to expose in structured output.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatusEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub response_received: bool,
    pub model_parsed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_states: Vec<SourceStateEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceStateEvidence {
    pub source: String,
    pub state: ObservationState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub state: ObservationState,
    pub source: String,
    pub observed_at: String,
    pub status_evidence: StatusEvidence,
}

impl Observation {
    pub fn new(
        state: ObservationState,
        source: impl Into<String>,
        status_evidence: StatusEvidence,
    ) -> Self {
        Self {
            state,
            source: source.into(),
            observed_at: observation_timestamp(),
            status_evidence,
        }
    }

    pub fn confirmed_present(source: impl Into<String>, http_status: Option<u16>) -> Self {
        Self::new(
            ObservationState::ConfirmedPresent,
            source,
            StatusEvidence {
                http_status,
                response_received: true,
                model_parsed: true,
                source_states: Vec::new(),
            },
        )
    }

    pub fn confirmed_absent(source: impl Into<String>, http_status: Option<u16>) -> Self {
        Self::new(
            ObservationState::ConfirmedAbsent,
            source,
            StatusEvidence {
                http_status,
                response_received: true,
                model_parsed: true,
                source_states: Vec::new(),
            },
        )
    }

    pub fn temporarily_unavailable(
        source: impl Into<String>,
        http_status: Option<u16>,
        response_received: bool,
    ) -> Self {
        Self::new(
            ObservationState::TemporarilyUnavailable,
            source,
            StatusEvidence {
                http_status,
                response_received,
                model_parsed: false,
                source_states: Vec::new(),
            },
        )
    }

    pub fn unrecognized_response(source: impl Into<String>, http_status: Option<u16>) -> Self {
        Self::new(
            ObservationState::UnrecognizedResponse,
            source,
            StatusEvidence {
                http_status,
                response_received: true,
                model_parsed: false,
                source_states: Vec::new(),
            },
        )
    }

    pub fn conflicting_sources(evidence: Vec<SourceStateEvidence>) -> Self {
        Self::new(
            ObservationState::ConflictingSources,
            "multiple_authoritative_sources",
            StatusEvidence {
                http_status: None,
                response_received: true,
                model_parsed: true,
                source_states: evidence,
            },
        )
    }

    /// Reconcile independent authoritative reads without allowing ambiguity to become absence.
    pub fn reconcile(observations: &[Self]) -> Option<Self> {
        let present = observations
            .iter()
            .any(|observation| observation.state == ObservationState::ConfirmedPresent);
        let absent = observations
            .iter()
            .any(|observation| observation.state == ObservationState::ConfirmedAbsent);
        if present && absent {
            return Some(Self::conflicting_sources(
                observations
                    .iter()
                    .map(|observation| SourceStateEvidence {
                        source: observation.source.clone(),
                        state: observation.state,
                    })
                    .collect(),
            ));
        }
        observations
            .iter()
            .find(|observation| {
                observation.state == ObservationState::ConfirmedPresent
                    || observation.state == ObservationState::ConfirmedAbsent
            })
            .cloned()
            .or_else(|| observations.first().cloned())
    }

    pub const fn retry_classification(
        &self,
        operation: ObservationOperation,
    ) -> RetryClassification {
        let upstream_transient = matches!(self.state, ObservationState::TemporarilyUnavailable);
        let safe_to_retry = match operation {
            ObservationOperation::Read => matches!(
                self.state,
                ObservationState::TemporarilyUnavailable | ObservationState::UnrecognizedResponse
            ),
            ObservationOperation::Mutation | ObservationOperation::PostMutationVerification => {
                false
            }
        };
        RetryClassification {
            upstream_transient,
            safe_to_retry,
        }
    }
}

pub fn observation_timestamp() -> String {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification_depends_on_state_and_operation() {
        let unavailable = Observation::temporarily_unavailable("listing_detail", Some(503), true);
        assert_eq!(
            unavailable.retry_classification(ObservationOperation::Read),
            RetryClassification {
                upstream_transient: true,
                safe_to_retry: true,
            }
        );
        assert_eq!(
            unavailable.retry_classification(ObservationOperation::PostMutationVerification),
            RetryClassification {
                upstream_transient: true,
                safe_to_retry: false,
            }
        );

        let conflict = Observation::conflicting_sources(vec![]);
        assert_eq!(
            conflict.retry_classification(ObservationOperation::Read),
            RetryClassification::default()
        );
    }

    #[test]
    fn authoritative_disagreement_is_explicit_and_blocks_retry() {
        let detail = Observation::confirmed_absent("listing_detail", Some(404));
        let collection = Observation::confirmed_present("listing_collection", Some(200));
        let reconciled = Observation::reconcile(&[detail, collection]).unwrap();

        assert_eq!(reconciled.state, ObservationState::ConflictingSources);
        assert_eq!(reconciled.status_evidence.source_states.len(), 2);
        assert!(
            !reconciled
                .retry_classification(ObservationOperation::Mutation)
                .safe_to_retry
        );
    }

    #[test]
    fn later_confirmed_presence_resolves_delayed_consistency() {
        let absent = Observation::confirmed_absent("listing_detail", Some(404));
        let present = Observation::confirmed_present("listing_detail", Some(200));

        assert_eq!(Observation::reconcile(&[present.clone()]).unwrap(), present);
        assert_eq!(absent.state, ObservationState::ConfirmedAbsent);
    }
}
