use super::support::*;

#[tokio::test]
async fn publish_runs_the_complete_bounded_sequence() {
    let transport = FixtureTransport::new(successful_publish_responses());
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1", "one").await.unwrap();

    assert_eq!(published.listing_id, "draft-1");
    assert_eq!(published.state, "pending");
    assert_eq!(
        published.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy",
            "validate",
            "wait_for_images",
            "verify_revision",
            "patch_item_fields",
            "fetch_fresh_model",
            "update_recommerce",
            "apply_delivery",
            "observe_delivery",
            "fetch_product_context",
            "package_choice",
            "fetch_confirmation",
            "track_confirmation",
            "fetch_observed_listing"
        ]
    );
    assert!(published.warnings.is_empty());

    let requests = transport.requests();
    assert_eq!(requests.len(), 15);
    let observed_sequence = requests
        .iter()
        .map(|request| (request.method.clone(), request.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_sequence,
        [
            (Method::Get, "/search?limit=50&offset=0"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (Method::Get, "/categories/taxonomy"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Patch, "/items/draft-1"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Put, "/adinput/ad/recommerce/draft-1/update"),
            (Method::Post, "/ads/draft-1/delivery"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (
                Method::Get,
                "/adinput/product/recommerce/draft-1/productcontext?adRevision=revision-7"
            ),
            (Method::Post, "/adinput/order/choices/draft-1"),
            (Method::Get, "/orders/4/confirmation/draft-1"),
            (
                Method::Get,
                "/tracking/adconfirmation?adId=draft-1&orderId=4"
            ),
            (Method::Get, "/draft-1"),
        ]
    );
    assert_eq!(
        requests[8].body,
        RequestBody::Json(json!({
            "meetup": true,
            "shipping": false,
            "sellerPaysShipping": false,
            "client": "ANDROID",
            "buyNow": false
        }))
    );
    assert_eq!(requests[5].if_match.as_deref(), Some("one"));
    assert_eq!(
        requests[5].body,
        RequestBody::Json(json!({
            "data": {
                "title": "Chair",
                "description": "Solid birch chair",
                "price": { "price_amount": 45 }
            }
        }))
    );
    assert_eq!(requests[7].if_match.as_deref(), Some("two"));
    let RequestBody::Json(recommerce_values) = &requests[7].body else {
        panic!("expected recommerce update")
    };
    assert_eq!(
        recommerce_values["price"],
        json!([{ "price_amount": "45" }])
    );
    assert_eq!(
        requests[11].body,
        RequestBody::Form(vec![(
            "choices".to_owned(),
            "urn:product:package-specification:10".to_owned()
        )])
    );
    assert!(requests.iter().all(|request| {
        !matches!(
            request.method,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        ) || request.retry == RetryPolicy::Never
    }));
}
#[tokio::test]
async fn publish_polls_detail_then_uses_the_active_collection_fallback() {
    let mut responses = successful_publish_responses();
    responses[13] = response(404, json!({ "message": "detail pending" }));
    responses.push(response(404, json!({ "message": "detail pending" })));
    let transport = FixtureTransport::new(responses).with_search_responses([
        response(200, json!({ "summaries": [], "total": 0 })),
        response(200, json!({ "summaries": [], "total": 0 })),
        listing_collection("draft-1", "ACTIVE"),
    ]);
    let mut workflow_config = config();
    workflow_config.listing_poll_limit = 1;
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), workflow_config)
        .publish("draft-1", "one")
        .await
        .unwrap();

    assert_eq!(result.publication, "persisted");
    assert!(result.mutations_performed);
    assert_eq!(result.observed_listing["observation_source"], "collection");
    assert_eq!(
        result.public_url,
        "https://www.tori.fi/recommerce/forsale/item/draft-1"
    );
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.path == "/draft-1")
            .count(),
        2
    );
}
#[tokio::test]
async fn publish_uses_collection_when_detail_response_model_is_unexpected() {
    let mut responses = successful_publish_responses();
    responses[13] = response(200, json!({ "unexpected": true }));
    let transport = FixtureTransport::new(responses).with_search_responses([
        response(200, json!({ "summaries": [], "total": 0 })),
        listing_collection("draft-1", "ACTIVE"),
    ]);
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport), config())
        .publish("draft-1", "one")
        .await
        .unwrap();

    assert_eq!(result.publication, "persisted");
    assert_eq!(result.observed_listing["observation_source"], "collection");
}
#[tokio::test]
async fn publish_returns_idempotent_success_for_an_already_active_listing() {
    let transport =
        FixtureTransport::new([]).with_search_responses([listing_collection("46031010", "ACTIVE")]);
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
        .publish("46031010", "stale")
        .await
        .unwrap();

    assert_eq!(result.publication, "already_published");
    assert!(!result.mutations_performed);
    assert_eq!(result.state, "active");
    assert_eq!(result.completed_steps, ["check_active_listing"]);
    assert_eq!(
        result.public_url,
        "https://www.tori.fi/recommerce/forsale/item/46031010"
    );
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].method, Method::Get);
}
#[tokio::test]
async fn stale_publish_revision_is_a_read_only_conflict() {
    let mut responses = successful_publish_responses();
    responses.truncate(4);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "stale").await.unwrap_err();

    assert_eq!(error.code, "draft.revision_conflict");
    assert_eq!(
        error.details.as_ref().unwrap()["expected_revision"],
        "stale"
    );
    assert_eq!(error.details.as_ref().unwrap()["observed_revision"], "one");
    assert_eq!(error.details.as_ref().unwrap()["safe_to_retry"], false);
    assert_eq!(
        error.details.as_ref().unwrap()["next_action"],
        "flea tori draft show draft-1"
    );
    let recovery = error.recovery.unwrap();
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("verify_revision"));
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft validate draft-1"
        ]
    );
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn publish_detects_a_draft_changed_during_validation() {
    let mut responses = successful_publish_responses();
    responses[3].body["etag"] = json!("two");
    responses.truncate(4);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.revision_conflict");
    assert_eq!(error.details.as_ref().unwrap()["expected_revision"], "one");
    assert_eq!(error.details.as_ref().unwrap()["observed_revision"], "two");
    assert_eq!(transport.requests().len(), 5);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn publish_requires_an_authoritative_upstream_revision() {
    let state = draft(
        "",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let transport = FixtureTransport::new([response(200, state)]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.source.unwrap().details.unwrap()["reason"],
        "draft revision is unavailable"
    );
    assert_eq!(transport.requests().len(), 2);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn publication_model_failures_before_dispatch_are_not_mutation_uncertainty() {
    let transport =
        FixtureTransport::new([response(200, json!({ "model": { "sections": [{}] } }))]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_ne!(error.code, "mutation.uncertain");
    assert_eq!(error.source.unwrap().details.unwrap()["path"], "$");
    assert_eq!(transport.requests().len(), 2);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn fresh_model_requires_ad_after_the_item_patch_without_claiming_a_new_mutation() {
    let mut responses = successful_publish_responses();
    responses[5] = response(200, json!({ "model": { "sections": [{}] } }));
    responses.truncate(7);
    responses.push(response(
        503,
        json!({ "message": "observation unavailable" }),
    ));
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let recovery = error.recovery.unwrap();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(recovery.failed_stage.as_deref(), Some("fetch_fresh_model"));
    assert_eq!(recovery.item_patch, Some(RecoveryStatus::Persisted));
    assert_eq!(
        recovery.recommerce_update,
        Some(RecoveryStatus::Unattempted)
    );
    let mutations = transport
        .requests()
        .into_iter()
        .filter(|request| request.method != Method::Get)
        .collect::<Vec<_>>();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].path, "/items/draft-1");
}
#[tokio::test]
async fn recommerce_update_accepts_wrapped_and_flattened_success_shapes() {
    for wrapped in [false, true] {
        let mut responses = successful_publish_responses();
        if wrapped {
            let flat = responses[6].body.clone();
            responses[6].body = json!({ "ad": flat, "model": { "sections": [] } });
        }
        let published = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        )
        .publish("draft-1", "one")
        .await
        .unwrap();
        assert_eq!(published.revision, "revision-7");
    }
}
#[tokio::test]
async fn package_choice_parse_uncertainty_reports_distinct_persistence_stages() {
    let mut responses = successful_publish_responses();
    responses[10] = response(200, json!({ "is-completed": true }));
    responses.truncate(11);
    responses.extend([
        response(503, json!({ "message": "draft observation unavailable" })),
        response(404, json!({ "message": "listing unavailable" })),
    ]);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let recovery = error.recovery.unwrap();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(error.source.unwrap().details.unwrap()["path"], "$.order-id");
    assert_eq!(recovery.item_patch, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.recommerce_update, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.delivery, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.package_choice, Some(RecoveryStatus::Indeterminate));
    assert_eq!(recovery.confirmation, Some(RecoveryStatus::Unattempted));
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.path == "/adinput/order/choices/draft-1")
            .count(),
        1
    );
}
#[tokio::test]
async fn rejected_publication_mutations_preserve_stage_boundaries() {
    for (index, stage, item, update, delivery) in [
        (
            4,
            "patch_item_fields",
            RecoveryStatus::Rejected,
            RecoveryStatus::Unattempted,
            RecoveryStatus::Pending,
        ),
        (
            6,
            "update_recommerce",
            RecoveryStatus::Persisted,
            RecoveryStatus::Rejected,
            RecoveryStatus::Pending,
        ),
        (
            7,
            "apply_delivery",
            RecoveryStatus::Persisted,
            RecoveryStatus::Persisted,
            RecoveryStatus::Rejected,
        ),
    ] {
        let mut responses = successful_publish_responses();
        responses[index] = response(400, json!({ "message": "rejected" }));
        let error = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        )
        .publish("draft-1", "one")
        .await
        .unwrap_err();
        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.failed_stage.as_deref(), Some(stage));
        assert_eq!(recovery.item_patch, Some(item));
        assert_eq!(recovery.recommerce_update, Some(update));
        assert_eq!(recovery.delivery, Some(delivery));
        assert_eq!(recovery.package_choice, Some(RecoveryStatus::Unattempted));
    }
}
#[tokio::test]
async fn publish_waits_for_processing_images_and_reuses_the_validation_engine() {
    let mut responses = successful_publish_responses();
    responses[0].body["images"][0]["state"] = json!("processing");
    let mut ready = responses[0].body.clone();
    ready["images"][0]["state"] = json!("ready");
    responses.insert(3, response(200, ready));
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1", "one").await.unwrap();

    assert_eq!(published.listing_id, "draft-1");
    assert_eq!(transport.requests()[3].method, Method::Get);
}
#[tokio::test]
async fn publish_validation_rejects_missing_fields_and_missing_delivery() {
    let mut missing_title_values = publication_values();
    missing_title_values
        .as_object_mut()
        .unwrap()
        .remove("title");
    let missing_title = draft(
        "one",
        json!({
            "values": missing_title_values,
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, missing_title),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["missing"][0]["field"], "title");
    let recovery = error.recovery.unwrap();
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("validate"));
    assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
    assert_eq!(
        recovery.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy"
        ]
    );
    assert_eq!(
        recovery.next_safe_actions[0],
        "flea tori draft update draft-1 --title VALUE"
    );

    let no_delivery = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, no_delivery),
            response(200, delivery_page(None)),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["missing"][0]["field"], "delivery");
    assert_eq!(
        error.recovery.unwrap().next_safe_actions[0],
        "flea tori draft update draft-1 --delivery VALUE"
    );
}
#[tokio::test]
async fn truncated_composer_options_are_local_unverifiable_validation() {
    let truncated = draft(
        "one",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair",
                "description": "Solid birch chair",
                "trade_type": "sell",
                "price": 45,
                "postal_code": "00100",
                "condition": 49
            },
            "fields": [{
                "key": "condition",
                "label": "Condition",
                "type": "select",
                "requirement": "required",
                "status": "set",
                "value": 49,
                "section": "details",
                "option_count": 60,
                "options_returned": 1,
                "options_truncated": true
            }],
            "options": [{ "field": "condition", "value": 0, "label": "New" }],
            "required_fields": ["title", "delivery", "condition"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, truncated),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let details = error.details.unwrap();
    let recovery = error.recovery.unwrap();

    assert_eq!(details["unverifiable"][0]["field"], "condition");
    assert!(details.get("evidence_failures").is_none());
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("validate"));
    assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
    assert_eq!(
        recovery.next_safe_actions[0],
        "flea tori draft show draft-1"
    );
}
#[tokio::test]
async fn publish_marks_transient_validation_evidence_reads_as_unverifiable() {
    let valid = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );

    for (responses, failed_stage, action, completed_steps) in [
        (
            vec![
                response(200, valid.clone()),
                response(503, json!({ "message": "delivery unavailable" })),
                category_taxonomy("furniture/chairs", true),
            ],
            "fetch_delivery_options",
            "flea tori draft show draft-1",
            vec!["fetch_draft", "fetch_category_taxonomy"],
        ),
        (
            vec![
                response(200, valid.clone()),
                response(200, delivery_page(Some("pickup"))),
                response(503, json!({ "message": "taxonomy unavailable" })),
            ],
            "fetch_category_taxonomy",
            "flea tori category list",
            vec!["fetch_draft", "fetch_delivery_options"],
        ),
    ] {
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();
        let details = error.details.unwrap();
        let recovery = error.recovery.unwrap();

        assert_eq!(error.code, "draft.validation_failed");
        assert_eq!(
            details["evidence_failures"][0]["failed_stage"],
            failed_stage
        );
        assert_eq!(
            details["unverifiable"][0]["field"],
            details["evidence_failures"][0]["field"]
        );
        assert!(recovery.upstream_transient);
        assert!(recovery.safe_to_retry);
        assert_eq!(recovery.failed_stage.as_deref(), Some(failed_stage));
        assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
        assert_eq!(recovery.completed_steps, completed_steps);
        assert_eq!(recovery.next_safe_actions[0], action);
    }
}
#[tokio::test]
async fn corrected_validation_state_can_publish_successfully() {
    let mut missing_title_values = publication_values();
    missing_title_values
        .as_object_mut()
        .unwrap()
        .remove("title");
    let invalid = draft(
        "one",
        json!({
            "values": missing_title_values,
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let invalid_workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, invalid),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );

    let error = invalid_workflow
        .publish("draft-1", "one")
        .await
        .unwrap_err();
    assert!(!error.recovery.unwrap().safe_to_retry);

    let corrected_workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(successful_publish_responses())),
        config(),
    );
    let published = corrected_workflow.publish("draft-1", "one").await.unwrap();
    assert_eq!(published.listing_id, "draft-1");
}
#[tokio::test]
async fn publish_reports_failed_image_and_preserves_its_identity() {
    let failed = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{
                "image_id": "image-broken",
                "position": 0,
                "state": "failed",
                "failure": "unsupported image"
            }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, failed),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["invalid"][0]["field"], "images");
    assert_eq!(
        error.recovery.unwrap().completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy"
        ]
    );
}
#[tokio::test]
async fn publish_failures_report_each_completed_workflow_boundary() {
    let cases = [
        (0, &[][..], true),
        (
            3,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
            ][..],
            true,
        ),
        (
            4,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
            ][..],
            false,
        ),
        (
            5,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
            ][..],
            false,
        ),
        (
            6,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
            ][..],
            false,
        ),
        (
            7,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
            ][..],
            false,
        ),
        (
            8,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
            ][..],
            false,
        ),
        (
            9,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
                "observe_delivery",
            ][..],
            false,
        ),
        (
            13,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
                "observe_delivery",
                "fetch_product_context",
                "package_choice",
                "fetch_confirmation",
                "track_confirmation",
            ][..],
            false,
        ),
    ];

    for (failure_index, expected_steps, safe_to_retry) in cases {
        let mut responses = successful_publish_responses();
        responses[failure_index] = response(503, json!({ "message": "fixture failure" }));
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();
        let recovery = error.recovery.unwrap();
        assert_eq!(
            recovery.completed_steps, expected_steps,
            "failure index {failure_index}"
        );
        assert!(recovery.upstream_transient, "failure index {failure_index}");
        assert_eq!(
            recovery.safe_to_retry, safe_to_retry,
            "failure index {failure_index}"
        );
        if failure_index == 13 {
            assert_eq!(error.code, "publication.observation_uncertain");
            let details = error.details.unwrap();
            assert_eq!(
                details["listing_id"], "draft-1",
                "published listing identity must survive observation failure"
            );
            assert_eq!(details["publication"], "persisted");
            assert_eq!(details["observation_status"], "unavailable");
            assert_eq!(details["observation_attempts"], 1);
            assert!(details["observation_elapsed_ms"].is_number());
            assert_eq!(recovery.publication, Some(RecoveryStatus::Persisted));
            assert_eq!(
                recovery.observation.state,
                flea::domain::observation::ObservationState::TemporarilyUnavailable
            );
            assert_eq!(
                recovery.next_safe_actions,
                ["flea tori listing show draft-1"]
            );
        } else {
            assert_eq!(recovery.next_safe_actions, ["flea tori draft show draft-1"]);
        }
    }
}
#[tokio::test]
async fn uncertain_publication_observes_before_recommending_any_continuation() {
    for observation_available in [true, false] {
        let mut responses = successful_publish_responses();
        responses.truncate(11);
        responses[10] = response(503, json!({ "message": "publication unavailable" }));
        responses.push(if observation_available {
            response(
                200,
                draft(
                    "observed-publication",
                    json!({
                        "values": {
                            "category": "furniture/chairs",
                            "title": "Chair",
                            "delivery": ["pickup"],
                            "revision": "revision-7"
                        },
                        "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
                    }),
                ),
            )
        } else {
            response(503, json!({ "message": "observation unavailable" }))
        });
        responses.push(response(404, json!({ "message": "listing unavailable" })));
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();

        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.failed_stage.as_deref(), Some("package_choice"));
        assert_eq!(recovery.delivery, Some(RecoveryStatus::Persisted));
        assert_eq!(recovery.publication, Some(RecoveryStatus::Indeterminate));
        assert_eq!(
            recovery.next_safe_actions,
            [
                "flea tori draft show draft-1",
                "flea tori listing show draft-1"
            ]
        );
        assert!(recovery.destructive_actions.is_empty());
        if observation_available {
            assert_eq!(recovery.observation.status, ObservationStatus::Observed);
            assert!(recovery.observation.observed_at.is_some());
            assert_eq!(
                recovery.observed_etag.as_deref(),
                Some("observed-publication")
            );
            assert_eq!(recovery.observed_revision.as_deref(), Some("revision-7"));
        } else {
            assert_eq!(recovery.observation.status, ObservationStatus::Unavailable);
            assert!(recovery.observation.observed_at.is_some());
        }
    }
}
#[tokio::test]
async fn confirmation_follow_ups_are_best_effort_and_observation_still_runs() {
    let mut confirmation_failure = successful_publish_responses();
    confirmation_failure[11] = response(503, json!({ "message": "confirmation unavailable" }));
    confirmation_failure.remove(12);
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(confirmation_failure)),
        config(),
    )
    .publish("draft-1", "one")
    .await
    .unwrap();
    assert_eq!(
        result.warnings,
        ["confirmation fetch failed: confirmation unavailable"]
    );
    assert_eq!(
        result.completed_steps.last().unwrap(),
        "fetch_observed_listing"
    );

    let mut tracking_failure = successful_publish_responses();
    tracking_failure[12] = response(503, json!({ "message": "tracking unavailable" }));
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(tracking_failure)),
        config(),
    )
    .publish("draft-1", "one")
    .await
    .unwrap();
    assert_eq!(
        result.warnings,
        ["confirmation tracking failed: tracking unavailable"]
    );
    assert!(
        !result
            .completed_steps
            .iter()
            .any(|step| step == "track_confirmation")
    );
}
#[tokio::test]
async fn publish_timeout_bounds_a_hung_poll_request() {
    let processing = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(HangingPollTransport {
            responses: Mutex::new(VecDeque::from([
                response(200, processing),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
        }),
        WorkflowConfig {
            image_processing_timeout: Duration::from_millis(20),
            image_poll_interval: Duration::ZERO,
            image_poll_limit: usize::MAX,
            listing_observation_timeout: Duration::from_secs(1),
            listing_poll_interval: Duration::ZERO,
            listing_poll_limit: 0,
        },
    );

    let error = tokio::time::timeout(Duration::from_secs(1), workflow.publish("draft-1", "one"))
        .await
        .expect("workflow must enforce its own deadline")
        .unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
}
#[tokio::test]
async fn publish_timeout_is_bounded_and_recoverable() {
    let processing = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, processing.clone()),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
        response(200, processing.clone()),
        response(200, processing),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
    assert_eq!(
        recovery.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy",
            "validate"
        ]
    );
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft validate draft-1"
        ]
    );
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.images[0].status, RecoveryStatus::Pending);
}
