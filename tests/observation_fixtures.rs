use flea::domain::observation::{Observation, ObservationOperation, ObservationState};
use serde::Deserialize;

#[derive(Deserialize)]
struct DisagreementFixture {
    detail: Observation,
    collection: Observation,
}

#[test]
fn list_and_detail_disagreement_is_not_absence_or_mutation_permission() {
    let fixture: DisagreementFixture = serde_json::from_str(include_str!(
        "fixtures/observations/list-detail-disagreement.json"
    ))
    .unwrap();

    let observation = Observation::reconcile(&[fixture.detail, fixture.collection]).unwrap();

    assert_eq!(observation.state, ObservationState::ConflictingSources);
    assert_eq!(observation.source, "multiple_authoritative_sources");
    assert_eq!(observation.status_evidence.source_states.len(), 2);
    assert!(
        !observation
            .retry_classification(ObservationOperation::Mutation)
            .safe_to_retry
    );
}

#[test]
fn a_confirmed_later_read_resolves_delayed_consistency_without_replaying_a_mutation() {
    let first = Observation::confirmed_absent("listing_detail", Some(404));
    let later = Observation::confirmed_present("listing_detail", Some(200));

    let resolved = Observation::reconcile(std::slice::from_ref(&later)).unwrap();

    assert_eq!(first.state, ObservationState::ConfirmedAbsent);
    assert_eq!(resolved.state, ObservationState::ConfirmedPresent);
    assert!(
        !resolved
            .retry_classification(ObservationOperation::PostMutationVerification)
            .safe_to_retry
    );
}
