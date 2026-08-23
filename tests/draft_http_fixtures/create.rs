use super::support::*;

#[tokio::test]
async fn creation_normalizes_the_source_observed_json_success() {
    let transport = FixtureTransport::new([response(
        200,
        observed_draft("98231", "W/\"7\"", json!({ "title": "Chair" })),
    )]);
    let api = HttpAdInputApi::new(transport.clone());

    let state = api.create_draft().await.unwrap();

    assert_eq!(state.draft_id, "98231");
    assert_eq!(state.etag, "W/\"7\"");
    assert_eq!(state.values["title"], "Chair");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::Post);
    assert_eq!(requests[0].path, "/adinput/ad/withModel/recommerce");
    assert_eq!(requests[0].body, RequestBody::Empty);
    assert_eq!(requests[0].retry, RetryPolicy::Never);
}
#[tokio::test]
async fn creation_observes_location_and_see_other_identity_without_reposting() {
    for status in [201, 303] {
        let transport = FixtureTransport::new([
            response_with_location(
                status,
                "https://apps-adinput.svc.tori.fi/adinput/ad/recommerce/98231",
            ),
            response(200, observed_draft("98231", "W/\"8\"", json!({}))),
        ]);
        let api = HttpAdInputApi::new(transport.clone());

        let state = api.create_draft().await.unwrap();

        assert_eq!(state.draft_id, "98231");
        assert_eq!(state.etag, "W/\"8\"");
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, Method::Post);
        assert_eq!(requests[1].method, Method::Get);
        assert_eq!(requests[1].path, "/adinput/ad/withModel/98231");
    }
}
#[tokio::test]
async fn uncertain_creation_covers_unparseable_empty_and_missing_identity_successes() {
    let mut non_json = response(200, Value::Null);
    non_json.body_is_unparseable = true;
    non_json.content_type = Some("text/html".to_owned());
    let mut malformed_json = response(200, Value::Null);
    malformed_json.body_is_unparseable = true;
    for response in [
        non_json,
        malformed_json,
        response(204, Value::Null),
        response(200, json!({})),
    ] {
        let transport = FixtureTransport::new([response]);
        let api = HttpAdInputApi::new(transport.clone());

        let error = api.create_draft().await.unwrap_err();

        assert_eq!(error.code, "mutation.uncertain");
        assert!(!error.upstream_transient);
        assert!(!error.safe_to_retry);
        let details = error.details.unwrap();
        assert_eq!(details["stage"], "create_draft");
        assert_eq!(details["completed_steps"], json!([]));
        assert!(
            details["recovery_guidance"]
                .as_str()
                .unwrap()
                .contains("do not repeat")
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.requests()[0].retry, RetryPolicy::Never);
    }
}
#[tokio::test]
async fn creation_observation_failure_preserves_authoritative_identity() {
    let transport = FixtureTransport::new([
        response_with_location(201, "/adinput/ad/recommerce/98231"),
        response(503, json!({ "message": "observation unavailable" })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .create(Map::new(), &[] as &[&str])
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.create_incomplete");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "98231");
    assert_eq!(
        recovery.completed_steps,
        ["create_draft", "establish_identity"]
    );
    assert_eq!(recovery.next_safe_actions, ["flea tori draft show 98231"]);
    assert_eq!(
        recovery.create.unwrap().allocation,
        RecoveryStatus::Persisted
    );
}
#[tokio::test]
async fn create_preflight_rejects_invalid_field_shapes_before_allocation() {
    let transport = FixtureTransport::new([]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .create(
            Map::from_iter([("title".to_owned(), json!({ "nested": true }))]),
            &[] as &[&str],
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.as_ref().unwrap()["stage"], "create_preflight");
    assert_eq!(error.details.as_ref().unwrap()["allocation"], "unattempted");
    assert!(error.recovery.is_none());
    assert!(transport.requests().is_empty());
}
#[tokio::test]
async fn creation_rejects_noncanonical_redirects_without_following_them() {
    let mut redirected = response_with_location(307, "https://example.com/draft/98231");
    redirected.body = json!({ "message": "redirect" });
    let transport = FixtureTransport::new([redirected]);
    let api = HttpAdInputApi::new(transport.clone());

    let error = api.create_draft().await.unwrap_err();

    assert_eq!(error.status, Some(307));
    assert_eq!(transport.requests().len(), 1);
}
#[tokio::test]
async fn source_backed_shape_validation_preserves_unrelated_field_mutations() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": { "title": 10 },
                    "fields": [decimal_field("title")]
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": { "title": 10, "postal_code": "00100" },
                    "fields": [decimal_field("title")]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.as_ref().unwrap()["stage"], "validate_title");
    assert_eq!(error.details.as_ref().unwrap()["fields"], json!(["title"]));
    assert_eq!(
        error.details.as_ref().unwrap()["field_errors"][0]["source"],
        "local_schema"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["title"]);
    assert_eq!(recovery.persisted_fields, ["postal_code"]);
    assert!(recovery.unattempted_fields.is_empty());
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(transport.requests()[0].method, Method::Get);
    assert_eq!(transport.requests()[1].method, Method::Put);
}
#[tokio::test]
async fn normalized_trade_type_validates_against_string_and_numeric_machine_options() {
    for machine_value in [json!("1"), json!(1)] {
        let model = json!({
            "values": { "trade_type": machine_value.clone() },
            "fields": [select_field("trade_type", machine_value.clone())],
            "options": [
                { "field": "trade_type", "value": machine_value.clone(), "label": "Sell" }
            ]
        });
        let transport = FixtureTransport::new([
            response(200, draft("one", model.clone())),
            response(200, draft("two", model)),
        ]);
        let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

        let result = workflow
            .update(
                "draft-1",
                &Map::from_iter([("trade_type".to_owned(), json!("sell"))]),
            )
            .await
            .unwrap();

        assert_eq!(result.persisted_fields, ["trade_type"]);
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        let RequestBody::Json(body) = &requests[1].body else {
            panic!("expected composer update body")
        };
        assert_eq!(body["trade_type"], "1");
    }
}
#[tokio::test]
async fn unavailable_and_unknown_trade_types_have_coherent_recovery() {
    for requested in [json!("give_away"), json!("4")] {
        let transport = FixtureTransport::new([response(
            200,
            draft(
                "one",
                json!({
                    "values": { "trade_type": requested.clone() },
                    "fields": [select_field("trade_type", requested.clone())],
                    "options": [
                        { "field": "trade_type", "value": "1", "label": "Sell" }
                    ]
                }),
            ),
        )]);
        let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

        let error = workflow
            .update(
                "draft-1",
                &Map::from_iter([("trade_type".to_owned(), requested)]),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "draft.validation_failed");
        let recovery = error.recovery.unwrap();
        assert!(recovery.persisted_fields.is_empty());
        assert_eq!(recovery.absent_fields, ["trade_type"]);
        assert_eq!(recovery.field_summary.len(), 1);
        assert_eq!(recovery.field_summary[0].field, "trade_type");
        assert_eq!(recovery.field_summary[0].status, RecoveryStatus::Absent);
        assert_eq!(transport.requests().len(), 1);
    }
}
#[tokio::test]
async fn create_accepts_a_category_outside_the_compact_composer_page() {
    let option_ids = (0..593).map(|index| index.to_string()).collect::<Vec<_>>();
    let mut initial = composer_with_category_options("1", &option_ids);
    initial["ad"]["values"]
        .as_object_mut()
        .unwrap()
        .remove("category");
    let transport = FixtureTransport::new([
        response(201, initial),
        category_taxonomy("46", true),
        response(200, composer_with_category_options("46", &option_ids)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let created = workflow
        .create(
            Map::from_iter([("category".to_owned(), json!(46))]),
            &[] as &[&str],
        )
        .await
        .unwrap();

    assert_eq!(created.draft.values["category"], json!("46"));
    let category = created
        .draft
        .fields
        .iter()
        .find(|field| field.key == "category")
        .unwrap();
    assert_eq!(category.option_count, 593);
    assert_eq!(category.options_returned, 50);
    assert!(category.options_truncated);
    assert_eq!(transport.requests()[1].path, "/categories/taxonomy");
}
