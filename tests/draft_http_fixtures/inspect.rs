use super::support::*;

#[tokio::test]
async fn draft_detail_absence_reconciles_with_the_authenticated_collection() {
    let transport = FixtureTransport::new([response(404, json!({ "message": "missing" }))])
        .with_search_responses([listing_collection("draft-1", "DRAFT")]);
    let api = HttpAdInputApi::new(transport.clone());

    let error = api.get_draft("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.observation_conflict");
    let observation = error.observation.unwrap();
    assert_eq!(observation.state, ObservationState::ConflictingSources);
    assert_eq!(observation.status_evidence.source_states.len(), 2);
    assert_eq!(
        observation.status_evidence.source_states[0].source,
        "draft_detail"
    );
    assert_eq!(
        observation.status_evidence.source_states[1].source,
        "authenticated_listing_collection"
    );
    assert_eq!(transport.requests().len(), 2);
}
#[tokio::test]
async fn collection_absence_confirms_a_deleted_draft() {
    let transport = FixtureTransport::new([response(404, json!({ "message": "missing" }))])
        .with_search_responses([response(200, json!({ "summaries": [], "total": 0 }))]);
    let api = HttpAdInputApi::new(transport);

    let error = api.get_draft("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.not_found");
    let observation = error.observation.unwrap();
    assert_eq!(observation.state, ObservationState::ConfirmedAbsent);
    assert_eq!(observation.source, "draft_lifecycle_reconciliation");
    assert_eq!(observation.status_evidence.source_states.len(), 2);
}
#[tokio::test]
async fn unavailable_collection_cannot_confirm_detail_absence() {
    let transport = FixtureTransport::new([response(404, json!({ "message": "missing" }))])
        .with_search_responses([response(
            503,
            json!({ "message": "collection unavailable" }),
        )]);
    let api = HttpAdInputApi::new(transport);

    let error = api.get_draft("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.observation_incomplete");
    assert_eq!(
        error.observation.unwrap().state,
        ObservationState::TemporarilyUnavailable
    );
}
#[tokio::test]
async fn a_later_detail_read_resolves_collection_consistency() {
    let transport = FixtureTransport::new([
        response(404, json!({ "message": "missing" })),
        response(200, draft("post-image", json!({}))),
    ])
    .with_search_responses([listing_collection("draft-1", "DRAFT")]);
    let api = HttpAdInputApi::new(transport);

    let first = api.get_draft("draft-1").await.unwrap_err();
    let later = api.get_draft("draft-1").await.unwrap();

    assert_eq!(
        first.observation.unwrap().state,
        ObservationState::ConflictingSources
    );
    assert_eq!(later.revision.as_deref(), Some("post-image"));
}
#[tokio::test]
async fn show_fetches_predictions_only_for_uncategorized_draft_with_images() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [{
                        "image_id": "image-1",
                        "position": 0,
                        "state": "ready"
                    }]
                }),
            ),
        ),
        response(
            200,
            json!([{ "category": "furniture/chairs", "confidence": 0.92 }]),
        ),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.show("draft-1").await.unwrap();

    assert_eq!(state.predictions[0].category, "furniture/chairs");
    assert_eq!(state.delivery.unwrap().options[0].value, "pickup");
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == Method::Get));
    assert!(
        requests
            .iter()
            .all(|request| request.retry == RetryPolicy::BoundedRead)
    );
}
#[tokio::test]
async fn validate_reports_complete_drafts_and_uses_only_read_requests() {
    let state = draft(
        "one",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, state),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let report = workflow.validate("draft-1").await.unwrap();

    assert!(report.ready);
    assert_eq!(report.revision, "one");
    assert!(report.missing.is_empty());
    assert!(report.invalid.is_empty());
    assert!(report.pending.is_empty());
    assert!(report.unverifiable.is_empty());
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request.method == Method::Get && request.retry == RetryPolicy::BoundedRead
    }));
}
#[tokio::test]
async fn show_and_validate_expose_the_same_authoritative_revision() {
    let state = draft(
        "revision-42",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let shown = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state.clone()),
            response(200, delivery_page(Some("pickup"))),
        ])),
        config(),
    )
    .show("draft-1")
    .await
    .unwrap();
    let validated = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();

    assert_eq!(shown.revision.as_deref(), Some("revision-42"));
    assert_eq!(validated.revision, "revision-42");
}
#[tokio::test]
async fn validate_applies_category_specific_composer_requirements() {
    let mut composer = composer_fixture();
    composer["ad"]["values"]
        .as_object_mut()
        .unwrap()
        .remove("condition");
    composer["ad"]["values"]["multi_image"] = json!([{
        "url": "https://img.tori.net/dynamic/default/image-1.jpg",
        "width": 640,
        "height": 480,
        "type": "image/jpeg"
    }]);
    composer["model"]["sections"][2]["content"][1]["required"] = json!(true);
    composer["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "hidden_serial",
            "type": "simple",
            "required": true,
            "hidden": true
        }));
    let mut delivery = delivery_fixture();
    delivery["context"]["meetup"] = json!(true);
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, composer),
            response(200, delivery),
            category_taxonomy("258", true),
        ])),
        config(),
    );

    let report = workflow.validate("46000000").await.unwrap();

    assert!(!report.ready);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].field, "condition");
    assert_eq!(report.missing[0].source, "listing_composer");
}
#[tokio::test]
async fn validate_distinguishes_giveaway_sale_delivery_and_image_states() {
    let mut giveaway_values = publication_values();
    giveaway_values["trade_type"] = json!("give_away");
    giveaway_values.as_object_mut().unwrap().remove("price");
    let giveaway = draft(
        "one",
        json!({
            "values": giveaway_values,
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, giveaway),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(report.ready);

    let mut sale_values = publication_values();
    sale_values.as_object_mut().unwrap().remove("price");
    let sale = draft(
        "one",
        json!({
            "values": sale_values,
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, sale),
            response(200, delivery_page(None)),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(report.missing.iter().any(|issue| issue.field == "price"));
    assert!(report.missing.iter().any(|issue| issue.field == "delivery"));
    assert!(report.pending.iter().any(|issue| issue.field == "images"));
    for issue in report.missing.iter().chain(&report.pending) {
        assert!(!issue.reason.is_empty());
        assert!(!issue.source.is_empty());
        assert!(issue.command.starts_with("flea "));
    }
}
#[tokio::test]
async fn validate_separates_missing_pending_and_rejected_images() {
    for (images, status) in [
        (json!([]), "missing"),
        (
            json!([{ "image_id": "image-1", "position": 0, "state": "processing" }]),
            "pending",
        ),
        (
            json!([{ "image_id": "image-1", "position": 0, "state": "failed" }]),
            "invalid",
        ),
    ] {
        let state = draft(
            "one",
            json!({ "values": publication_values(), "images": images }),
        );
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, state),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
            config(),
        )
        .validate("draft-1")
        .await
        .unwrap();
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value[status][0]["field"], "images");
    }
}
#[tokio::test]
async fn validate_reports_unselectable_and_unavailable_authoritative_models() {
    let state = draft(
        "one",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state.clone()),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", false),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert_eq!(report.invalid[0].field, "category");
    assert_eq!(report.invalid[0].source, "category_taxonomy");

    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state),
            response(503, json!({ "message": "delivery unavailable" })),
            response(503, json!({ "message": "taxonomy unavailable" })),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(
        report
            .unverifiable
            .iter()
            .any(|issue| issue.field == "category")
    );
    assert!(
        report
            .unverifiable
            .iter()
            .any(|issue| issue.field == "delivery")
    );
}
#[tokio::test]
async fn validate_marks_unavailable_and_malformed_models_unverifiable() {
    for model in [Value::Null, json!({ "sections": "invalid" })] {
        let mut values = publication_values();
        values["multi_image"] = json!([{
            "url": "https://img.tori.net/dynamic/default/image-1.jpg",
            "width": 640,
            "height": 480,
            "type": "image/jpeg"
        }]);
        let source = json!({
            "ad": {
                "id": "draft-1",
                "etag": "one",
                "values": values,
                "meta-data": {},
                "locked-fields": []
            },
            "model": model
        });
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, source),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
            config(),
        )
        .validate("draft-1")
        .await
        .unwrap();
        assert!(!report.ready);
        assert_eq!(report.unverifiable[0].field, "composer_model");
        assert_eq!(report.unverifiable[0].source, "listing_composer");
    }
}
#[tokio::test]
async fn source_observed_composer_exposes_required_fields_price_and_revision() {
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, composer_fixture())]));

    let state = api.get_draft("46000000").await.unwrap();

    assert_eq!(state.revision.as_deref(), Some("12345"));
    assert_eq!(state.values["price"], 25);
    assert_eq!(
        state.required_fields,
        [
            "trade_type",
            "category",
            "title",
            "description",
            "postal-code"
        ]
    );
    let title = state
        .fields
        .iter()
        .find(|field| field.key == "title")
        .unwrap();
    assert_eq!(title.label, "Ilmoituksen otsikko");
    assert_eq!(title.field_type, FieldType::String);
    assert_eq!(title.requirement, Requirement::Required);
    assert_eq!(title.status, FieldStatus::Set);
    assert_eq!(title.value, Some(json!("Polkupyörän vaijerilukko")));
    let price = state
        .fields
        .iter()
        .find(|field| field.key == "price_amount")
        .unwrap();
    assert_eq!(price.field_type, FieldType::Decimal);
    assert_eq!(price.value, Some(json!(25)));
    let postal_code = state
        .fields
        .iter()
        .find(|field| field.key == "postal-code")
        .unwrap();
    assert_eq!(postal_code.value, Some(json!("00100")));
    assert!(state.fields.iter().all(|field| field.key != "price_max"));
    assert!(state.fields.iter().all(|field| field.key != "bikes_type"));
}
#[tokio::test]
async fn show_merges_source_observed_delivery_options_with_machine_values() {
    let transport = FixtureTransport::new([
        response(200, composer_fixture()),
        response(200, delivery_fixture()),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.show("46000000").await.unwrap();

    let delivery = state
        .fields
        .iter()
        .find(|field| field.key == "delivery")
        .unwrap();
    assert_eq!(delivery.field_type, FieldType::MultiSelect);
    assert_eq!(delivery.requirement, Requirement::Required);
    assert_eq!(delivery.status, FieldStatus::Missing);
    assert_eq!(delivery.value, Some(json!([])));
    assert_eq!(delivery.option_count, 4);
    assert_eq!(delivery.options_returned, 4);
    assert!(!delivery.options_truncated);
    assert!(state.required_fields.contains(&"delivery".to_owned()));
    assert_eq!(state.values["delivery"], json!([]));
    assert_eq!(
        state
            .options
            .iter()
            .filter(|option| option.field == "delivery")
            .map(|option| option.value.clone())
            .collect::<Vec<_>>(),
        [
            json!("pickup"),
            json!("shipping:small"),
            json!("shipping:medium"),
            json!("shipping:large"),
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].path,
        "/ui/addelivery?adId=46000000&editMode=false"
    );
}
#[tokio::test]
async fn publish_validates_the_same_source_observed_model_as_show() {
    let transport = FixtureTransport::new([
        response(200, composer_fixture()),
        response(200, delivery_fixture()),
        category_taxonomy("258", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("46000000", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let details = error.details.unwrap();
    assert_eq!(details["missing"][0]["field"], "delivery");
    assert_eq!(details["missing"][1]["field"], "images");
    assert_eq!(transport.requests().len(), 4);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn composer_bounds_options_and_reports_truncation() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"][1]["items"] = Value::Array(
        (0..60)
            .map(|index| json!({ "label": format!("Condition {index}"), "value": index }))
            .collect(),
    );
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let state = api.get_draft("46000000").await.unwrap();

    let condition = state
        .fields
        .iter()
        .find(|field| field.key == "condition")
        .unwrap();
    assert_eq!(condition.option_count, 60);
    assert_eq!(condition.options_returned, 50);
    assert!(condition.options_truncated);
    assert_eq!(
        state
            .options
            .iter()
            .filter(|option| option.field == "condition")
            .count(),
        50
    );
}
#[tokio::test]
async fn inspection_expansion_returns_the_complete_composer_option_set() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"][1]["items"] = Value::Array(
        (0..60)
            .map(|index| json!({ "label": format!("Condition {index}"), "value": index }))
            .collect(),
    );
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let publication = api
        .publication_draft_for_inspection("46000000", true)
        .await
        .unwrap();
    let condition = publication
        .draft
        .fields
        .iter()
        .find(|field| field.key == "condition")
        .unwrap();

    assert_eq!(condition.option_count, 60);
    assert_eq!(condition.options_returned, 60);
    assert!(!condition.options_truncated);
    assert_eq!(
        publication
            .draft
            .options
            .iter()
            .filter(|option| option.field == "condition")
            .count(),
        60
    );
}
#[tokio::test]
async fn composer_keeps_a_selected_category_outside_the_option_bound() {
    let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
    let api = HttpAdInputApi::new(FixtureTransport::new([response(
        200,
        composer_with_category_options("59", &option_ids),
    )]));

    let state = api.get_draft("46000000").await.unwrap();
    let category = state
        .fields
        .iter()
        .find(|field| field.key == "category")
        .unwrap();
    let options = state
        .options
        .iter()
        .filter(|option| option.field == "category")
        .collect::<Vec<_>>();

    assert_eq!(category.option_count, 60);
    assert_eq!(category.options_returned, 50);
    assert!(category.options_truncated);
    assert_eq!(options.len(), 50);
    assert!(options.iter().any(|option| option.value == json!("59")));
    assert_eq!(options.last().unwrap().label, "Category 59");
}
#[tokio::test]
async fn inspection_option_limits_do_not_change_category_validation() {
    for include_all_options in [false, true] {
        let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
        let (state, report) = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, composer_with_category_options("59", &option_ids)),
                response(200, delivery_fixture()),
                category_taxonomy("59", true),
            ])),
            config(),
        )
        .inspect("46000000", include_all_options)
        .await
        .unwrap();

        assert_eq!(
            report
                .category_validation
                .as_ref()
                .and_then(|category| category.compatible),
            Some(true)
        );
        let category = state
            .fields
            .iter()
            .find(|field| field.key == "category")
            .unwrap();
        assert_eq!(category.options_truncated, !include_all_options);
        assert!(
            state
                .options
                .iter()
                .any(|option| { option.field == "category" && option.value == json!("59") })
        );
    }
}
#[tokio::test]
async fn validate_uses_exact_taxonomy_lookup_inside_and_outside_the_option_bound() {
    for selected in ["1", "59"] {
        let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, composer_with_category_options(selected, &option_ids)),
                response(200, delivery_fixture()),
                category_taxonomy(selected, true),
            ])),
            config(),
        )
        .validate("46000000")
        .await
        .unwrap();

        let category = report.category_validation.as_ref().unwrap();
        assert_eq!(category.value, selected);
        assert_eq!(category.label.as_deref(), Some("Fixture category"));
        assert_eq!(category.exists, Some(true));
        assert_eq!(category.selectable, Some(true));
        assert_eq!(category.compatible, Some(true));
        assert_eq!(
            category.existence_source.as_deref(),
            Some("category_taxonomy")
        );
        assert_eq!(
            category.selectability_source.as_deref(),
            Some("category_taxonomy")
        );
        assert_eq!(
            category.compatibility_source.as_deref(),
            Some("listing_composer")
        );
        assert!(report.invalid.iter().all(|issue| issue.field != "category"));
        assert!(
            report
                .unverifiable
                .iter()
                .all(|issue| issue.field != "category")
        );
    }
}
#[tokio::test]
async fn validate_rejects_removed_categories_with_an_exact_recovery_lookup() {
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(
                200,
                composer_with_category_options("258", &["258".to_owned()]),
            ),
            response(200, delivery_fixture()),
            category_taxonomy("999", true),
        ])),
        config(),
    )
    .validate("46000000")
    .await
    .unwrap();

    let category = report.category_validation.as_ref().unwrap();
    assert_eq!(category.exists, Some(false));
    assert_eq!(category.selectable, None);
    assert_eq!(category.compatible, Some(true));
    let issue = report
        .invalid
        .iter()
        .find(|issue| issue.field == "category")
        .unwrap();
    assert!(issue.reason.contains("absent from or inaccessible"));
    assert_eq!(issue.command, "flea tori category search 258");
}
#[tokio::test]
async fn validate_rejects_category_schema_disagreement() {
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(
                200,
                composer_with_category_options("258", &["257".to_owned()]),
            ),
            response(200, delivery_fixture()),
            category_taxonomy("258", true),
        ])),
        config(),
    )
    .validate("46000000")
    .await
    .unwrap();

    let category = report.category_validation.as_ref().unwrap();
    assert_eq!(category.exists, Some(true));
    assert_eq!(category.selectable, Some(true));
    assert_eq!(category.compatible, Some(false));
    assert!(report.invalid.iter().any(|issue| {
        issue.field == "category"
            && issue.source == "listing_composer"
            && issue.reason.contains("incompatible")
    }));
}
#[tokio::test]
async fn composer_preserves_safe_unknown_types_without_protocol_details() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "future_gain",
            "type": "future-slider",
            "sub-type": "v2",
            "label": "Future gain",
            "required": false,
            "owner": { "email": "owner@example.invalid" },
            "protocol-token": "protocol-secret"
        }));
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let state = api.get_draft("46000000").await.unwrap();

    let unknown = state
        .fields
        .iter()
        .find(|field| field.key == "future_gain")
        .unwrap();
    assert_eq!(
        unknown.field_type,
        FieldType::Unknown("future-slider".to_owned())
    );
    assert_eq!(
        unknown.raw,
        Some(json!({
            "type": "future-slider",
            "sub_type": "v2",
            "has_children": false,
            "has_options": false
        }))
    );
    let output = serde_json::to_string(&state).unwrap();
    assert!(!output.contains("owner@example.invalid"));
    assert!(!output.contains("protocol-secret"));
}
#[tokio::test]
async fn composer_distinguishes_empty_malformed_and_evolved_models() {
    let mut malformed = composer_fixture();
    malformed["model"]
        .as_object_mut()
        .unwrap()
        .remove("sections");
    malformed["owner"] = json!({ "email": "owner@example.invalid" });
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, malformed)]));

    let error = api.get_draft("46000000").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.details.as_deref().unwrap()["path"],
        "$.model.sections"
    );
    assert!(!format!("{error:?}").contains("owner@example.invalid"));

    let unavailable = json!({
        "draft_id": "46000000",
        "etag": "12345",
        "values": {}
    });
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, unavailable)]));
    let error = api.get_draft("46000000").await.unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(error.details.as_deref().unwrap()["path"], "$.fields");

    let mut conflicting_revision = composer_fixture();
    conflicting_revision["ad"]["etag"] = json!("W/\"54321\"");
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, conflicting_revision)]));
    let error = api.get_draft("46000000").await.unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.details.as_deref().unwrap()["reason"],
        "draft revision sources disagree"
    );

    let mut empty = composer_fixture();
    empty["model"]["sections"] = json!([]);
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, empty)]));
    let state = api.get_draft("46000000").await.unwrap();
    assert!(state.fields.is_empty());
    assert!(state.options.is_empty());
    assert!(state.required_fields.is_empty());

    let mut evolved = composer_fixture();
    let condition = evolved["model"]["sections"][2]["content"][1]
        .as_object_mut()
        .unwrap();
    let mut options = condition.remove("items").unwrap();
    options[0]["value"] = json!(1);
    condition.insert("options".to_owned(), options);
    evolved["model"]["schema-version"] = json!(2);
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, evolved)]));
    let state = api.get_draft("46000000").await.unwrap();
    assert!(state.options.iter().any(|option| {
        option.field == "condition" && option.value == json!(1) && option.label == "Uusi"
    }));
}
#[tokio::test]
async fn delivery_composer_bounds_options_and_reports_truncation() {
    let mut fixture = delivery_fixture();
    fixture["sections"]["shipping"]["packageSizes"] = Value::Object(
        (0..60)
            .map(|index| {
                (
                    format!("package-{index}"),
                    json!({
                        "title": format!("Package {index}"),
                        "size": format!("SIZE_{index}")
                    }),
                )
            })
            .collect(),
    );
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let composer = api.delivery_composer("46000000").await.unwrap();

    assert_eq!(composer.state.option_count, 61);
    assert_eq!(composer.state.options_returned, 50);
    assert!(composer.state.options_truncated);
    assert_eq!(composer.state.options.len(), 50);
}
