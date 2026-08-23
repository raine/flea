use super::support::*;

#[tokio::test]
async fn mixed_update_persists_prior_atomic_fields_before_invalid_select() {
    let initial_model = json!({
        "values": { "title": "Old", "trade_type": "1" },
        "fields": [select_field("trade_type", json!("1"))],
        "options": [
            { "field": "trade_type", "value": "1", "label": "Sell" },
            { "field": "trade_type", "value": "2", "label": "Give away" }
        ]
    });
    let updated_model = json!({
        "values": { "title": "Chair", "trade_type": "1" },
        "fields": [select_field("trade_type", json!("1"))],
        "options": [
            { "field": "trade_type", "value": "1", "label": "Sell" },
            { "field": "trade_type", "value": "2", "label": "Give away" }
        ]
    });
    let final_model = json!({
        "values": { "title": "Chair", "trade_type": "1", "postal_code": "00100" },
        "fields": [select_field("trade_type", json!("1"))],
        "options": [
            { "field": "trade_type", "value": "1", "label": "Sell" },
            { "field": "trade_type", "value": "2", "label": "Give away" }
        ]
    });
    let transport = FixtureTransport::new([
        response(200, draft("one", initial_model)),
        response(200, draft("two", updated_model)),
        response(200, draft("three", final_model)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("title".to_owned(), json!("Chair")),
                ("trade_type".to_owned(), json!("wanted")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.persisted_fields, ["postal_code", "title"]);
    assert_eq!(recovery.absent_fields, ["trade_type"]);
    assert!(recovery.unattempted_fields.is_empty());
    assert_eq!(transport.requests().len(), 3);
}
#[tokio::test]
async fn structured_upstream_errors_name_the_active_field_and_preserve_progress() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": { "title": "Old", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            422,
            json!({
                "errors": [{
                    "field": "item.price",
                    "code": "out_of_range",
                    "message": "Price is outside the accepted range"
                }]
            }),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let details = error.details.unwrap();
    assert_eq!(details["stage"], "apply_price");
    assert_eq!(details["fields"], json!(["price"]));
    assert_eq!(details["field_errors"][0]["field"], "price");
    assert_eq!(details["field_errors"][0]["code"], "out_of_range");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft", "apply_title"]);
    assert_eq!(recovery.persisted_fields, ["title"]);
    assert_eq!(recovery.absent_fields, ["price"]);
    assert_eq!(recovery.unattempted_fields, ["postal_code"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft update draft-1 --price VALUE"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, Method::Put);
    assert_eq!(requests[2].method, Method::Patch);
}
#[tokio::test]
async fn html_5xx_with_confirmed_field_persistence_continues_without_replaying() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": { "title": "Old", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        html_response(502),
        response(
            200,
            draft(
                "three",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 25 }
                }),
            ),
        ),
        response(
            200,
            draft(
                "four",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 25,
                        "postal_code": "00100"
                    }
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(result.persisted_fields, ["postal_code", "price", "title"]);
    assert!(result.ignored_fields.is_empty());
    assert_eq!(result.draft.etag, "four");
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("authoritative observation"));
    assert!(result.completed_steps.contains(&"observe_price".to_owned()));
    let requests = transport.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
    assert!(requests.iter().any(|request| {
        matches!(
            &request.body,
            RequestBody::Json(body)
                if body.pointer("/location/0/postal-code") == Some(&json!("00100"))
                    && body.get("postal_code").is_none()
        )
    }));
}
#[tokio::test]
async fn empty_and_missing_ad_successes_reconcile_title_and_description() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "values": { "title": "Old", "description": "Old" } }),
            ),
        ),
        response(200, Value::Null),
        response(
            200,
            draft(
                "two",
                json!({ "values": { "title": "Chair", "description": "Old" } }),
            ),
        ),
        response(200, json!({ "model": { "sections": [] } })),
        response(
            200,
            draft(
                "three",
                json!({ "values": { "title": "Chair", "description": "Comfortable" } }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("title".to_owned(), json!("Chair")),
                ("description".to_owned(), json!("Comfortable")),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(result.persisted_fields, ["description", "title"]);
    assert!(result.ignored_fields.is_empty());
    assert_eq!(result.draft.etag, "three");
    assert_eq!(result.warnings.len(), 2);
    assert_eq!(
        result.completed_steps,
        [
            "fetch_draft",
            "apply_title",
            "observe_title",
            "apply_description",
            "observe_description"
        ]
    );
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == Method::Put)
            .count(),
        2
    );
}
#[tokio::test]
async fn changed_etag_without_requested_content_remains_uncertain() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({ "values": { "title": "Old" } }))),
        response(200, Value::Null),
        response(200, draft("two", json!({ "values": { "title": "Other" } }))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("title".to_owned(), json!("Requested"))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(
        error.details.as_ref().unwrap()["observation"]["etag_changed"],
        true
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["title"]);
    assert!(recovery.persisted_fields.is_empty());
    assert!(recovery.indeterminate_fields.is_empty());
    assert_eq!(transport.requests().len(), 3);
}
#[tokio::test]
async fn unchanged_etag_proves_an_ambiguous_field_absent_and_limits_recovery() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
        html_response(502),
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(25))]),
        )
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["price"]);
    assert!(recovery.persisted_fields.is_empty());
    assert!(recovery.indeterminate_fields.is_empty());
    assert!(!recovery.safe_to_retry);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft update draft-1 --price VALUE"
        ]
    );
    assert_eq!(error.details.unwrap()["observation"]["etag_changed"], false);
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
}
#[tokio::test]
async fn failed_observation_keeps_the_active_field_indeterminate() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
        html_response(502),
        response(503, json!({ "message": "observation unavailable" })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(25))]),
        )
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.active_step.as_deref(), Some("apply_price"));
    assert_eq!(recovery.fields, ["price"]);
    assert_eq!(recovery.indeterminate_fields, ["price"]);
    assert!(recovery.absent_fields.is_empty());
    assert!(recovery.manual_inspection_required);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.next_safe_actions, ["flea tori draft show draft-1"]);
    assert_eq!(recovery.observation.status, ObservationStatus::Unavailable);
    assert!(recovery.destructive_actions.is_empty());
    assert_eq!(
        recovery.field_summary[0].status,
        RecoveryStatus::Indeterminate
    );
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
}
#[tokio::test]
async fn update_conflict_fetches_and_returns_fresh_remote_state() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({ "values": { "title": "old" } }))),
        response(412, json!({ "message": "etag mismatch" })),
        response(
            200,
            draft("two", json!({ "values": { "title": "other agent" } })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("title".to_owned(), json!("mine"))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.conflict");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.completed_steps, ["fetch_draft"]);
    assert!(!recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
    assert_eq!(
        recovery.observation.status,
        ObservationStatus::ChangedByAnotherClient
    );
    assert_eq!(recovery.observed_etag.as_deref(), Some("two"));
    assert_eq!(recovery.next_safe_actions, ["flea tori draft show draft-1"]);
    assert!(recovery.destructive_actions.is_empty());
    assert_eq!(recovery.fresh_state.unwrap().values["title"], "other agent");
    let requests = transport.requests();
    assert_eq!(requests[1].if_match.as_deref(), Some("one"));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
}
#[tokio::test]
async fn sale_price_uses_the_item_partial_update_and_authoritative_observation() {
    let item_response: Value =
        serde_json::from_str(include_str!("../fixtures/drafts/item-price-update.json")).unwrap();
    let observed_response: Value =
        serde_json::from_str(include_str!("../fixtures/drafts/priced-composer.json")).unwrap();
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "46031010",
                "W/\"1178961228\"",
                json!({ "category": 258, "trade_type": "1" }),
            ),
        ),
        response(200, item_response),
        response(200, observed_response),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "46031010",
            &Map::from_iter([("price".to_owned(), json!(5))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5));
    assert_eq!(state.draft.values["trade_type"], "1");
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(requests[1].method, Method::Patch);
    assert_eq!(requests[1].path, "/items/46031010");
    assert_eq!(requests[1].if_match.as_deref(), Some("W/\"1178961228\""));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(
        requests[1].body,
        RequestBody::Json(json!({
            "data": { "price": { "price_amount": 5 } }
        }))
    );
    assert_eq!(requests[2].method, Method::Get);
    assert_eq!(requests[2].retry, RetryPolicy::BoundedRead);
}
#[tokio::test]
async fn decimal_sale_price_preserves_the_requested_amount() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        response(200, item_update("draft-1", "two", json!(5.25))),
        response(
            200,
            observed_draft(
                "draft-1",
                "three",
                json!({ "trade_type": "1", "price": [{ "price_amount": "5.25" }] }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(5.25))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5.25));
    assert_eq!(
        transport.requests()[1].body,
        RequestBody::Json(json!({
            "data": { "price": { "price_amount": 5.25 } }
        }))
    );
}
#[tokio::test]
async fn creation_applies_fields_before_the_dedicated_sale_price() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        category_taxonomy("258", true),
        response(200, draft("two", json!({ "values": { "category": 258 } }))),
        response(
            200,
            draft(
                "three",
                json!({ "values": { "category": 258, "title": "Helmet" } }),
            ),
        ),
        response(
            200,
            draft(
                "four",
                json!({
                    "values": {
                        "category": 258,
                        "title": "Helmet",
                        "description": "Safe helmet"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "five",
                json!({
                    "values": {
                        "category": 258,
                        "title": "Helmet",
                        "description": "Safe helmet",
                        "trade_type": "1"
                    }
                }),
            ),
        ),
        response(200, item_update("draft-1", "six", json!(5))),
        response(
            200,
            observed_draft(
                "draft-1",
                "seven",
                json!({
                    "category": 258,
                    "title": "Helmet",
                    "description": "Safe helmet",
                    "trade_type": "1",
                    "price": [{ "price_amount": "5" }]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let created = workflow
        .create(
            Map::from_iter([
                ("category".to_owned(), json!(258)),
                ("title".to_owned(), json!("Helmet")),
                ("description".to_owned(), json!("Safe helmet")),
                ("trade_type".to_owned(), json!("1")),
                ("price".to_owned(), json!(5)),
            ]),
            &[] as &[&str],
        )
        .await
        .unwrap();

    assert_eq!(created.draft.values["price"], json!(5));
    assert_eq!(
        created.completed_steps,
        [
            "create_draft",
            "fetch_category_taxonomy",
            "apply_category",
            "apply_title",
            "apply_description",
            "apply_trade_type",
            "apply_price",
            "observe_price"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 8);
    assert_eq!(requests[1].path, "/categories/taxonomy");
    assert!(
        requests[2..6]
            .iter()
            .all(|request| request.method == Method::Put)
    );
    let RequestBody::Json(fields) = &requests[5].body else {
        panic!("expected composer field update")
    };
    assert_eq!(fields["trade_type"], "1");
    assert!(fields.get("price").is_none());
    assert_eq!(requests[6].method, Method::Patch);
    assert_eq!(requests[6].path, "/items/draft-1");
    assert_eq!(requests[6].retry, RetryPolicy::Never);
    assert_eq!(requests[7].method, Method::Get);
}
#[tokio::test]
async fn update_applies_field_groups_in_deterministic_protocol_order() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": {
                        "title": "Old",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(200, item_update("draft-1", "four", json!(25))),
        response(
            200,
            draft(
                "five",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 25,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "six",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 25,
                        "postal_code": "00100"
                    }
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("trade_type".to_owned(), json!("sell")),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(
        result.completed_steps,
        [
            "fetch_draft",
            "apply_title",
            "apply_trade_type",
            "apply_price",
            "observe_price",
            "apply_postal_code"
        ]
    );
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| &request.method)
            .collect::<Vec<_>>(),
        [
            &Method::Get,
            &Method::Put,
            &Method::Put,
            &Method::Patch,
            &Method::Get,
            &Method::Put,
        ]
    );
}
#[tokio::test]
async fn give_away_price_combinations_fail_before_mutating() {
    let create_transport = FixtureTransport::new([]);
    let create_workflow =
        DraftWorkflow::new(HttpAdInputApi::new(create_transport.clone()), config());
    let error = create_workflow
        .create(
            Map::from_iter([
                ("trade_type".to_owned(), json!("2")),
                ("price".to_owned(), json!(5)),
            ]),
            &[] as &[&str],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.price_trade_type_conflict");
    assert!(create_transport.requests().is_empty());

    let update_transport = FixtureTransport::new([response(
        200,
        observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
    )]);
    let update_workflow =
        DraftWorkflow::new(HttpAdInputApi::new(update_transport.clone()), config());
    let error = update_workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("trade_type".to_owned(), json!("2")),
                ("price".to_owned(), json!(5)),
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.price_trade_type_conflict");
    assert_eq!(update_transport.requests().len(), 1);
    assert_eq!(update_transport.requests()[0].method, Method::Get);
}
#[tokio::test]
async fn switching_to_give_away_omits_the_sale_price() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "draft-1",
                "one",
                json!({ "trade_type": "1", "price": [{ "price_amount": "5" }] }),
            ),
        ),
        response(
            200,
            observed_draft("draft-1", "two", json!({ "trade_type": "2" })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("trade_type".to_owned(), json!("2"))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["trade_type"], "2");
    assert!(state.draft.values.get("price").is_none());
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, Method::Put);
    let RequestBody::Json(values) = &requests[1].body else {
        panic!("expected composer update")
    };
    assert!(values.get("price").is_none());
}
#[tokio::test]
async fn composer_updates_reencode_an_observed_sale_price_without_currency_guessing() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "draft-1",
                "one",
                json!({
                    "trade_type": "1",
                    "price": [{ "price_amount": "5.25" }]
                }),
            ),
        ),
        response(
            200,
            observed_draft(
                "draft-1",
                "two",
                json!({
                    "trade_type": "1",
                    "title": "Updated",
                    "price": [{ "price_amount": "5.25" }]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("title".to_owned(), json!("Updated"))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5.25));
    let RequestBody::Json(values) = &transport.requests()[1].body else {
        panic!("expected composer update")
    };
    assert_eq!(values["price"], json!([{ "price_amount": "5.25" }]));
    assert!(values["price"][0].get("currency").is_none());
}
#[tokio::test]
async fn malformed_source_price_shapes_are_rejected() {
    let transport = FixtureTransport::new([response(
        200,
        observed_draft(
            "draft-1",
            "one",
            json!({ "trade_type": "1", "price": { "amount": 5 } }),
        ),
    )]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.show("draft-1").await.unwrap_err();

    assert_eq!(error.code, "upstream.unexpected_response");
    assert_eq!(
        error.source.unwrap().details.unwrap()["stage"],
        "normalize_price"
    );
}
#[tokio::test]
async fn price_failure_preserves_transience_and_unsafe_recovery_details() {
    let mut failed = response(502, Value::Null);
    failed.content_type = Some("text/html".to_owned());
    failed.body_is_unparseable = true;
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        failed,
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update("draft-1", &Map::from_iter([("price".to_owned(), json!(5))]))
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    let source = error.source.unwrap();
    assert_eq!(source.status, Some(502));
    assert!(source.upstream_transient);
    assert!(!source.safe_to_retry);
    assert_eq!(source.details.as_deref().unwrap()["stage"], "apply_price");
    assert_eq!(
        source.details.as_deref().unwrap()["content_type"],
        "text/html"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft"]);
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_price"));
    assert_eq!(recovery.absent_fields, ["price"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft update draft-1 --price VALUE"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests[1].method, Method::Patch);
    assert_eq!(requests[1].retry, RetryPolicy::Never);
}
#[tokio::test]
async fn price_success_requires_an_authoritative_matching_observation() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        response(200, item_update("draft-1", "two", json!(5))),
        response(
            200,
            observed_draft(
                "draft-1",
                "three",
                json!({ "trade_type": "1", "price": [{ "price_amount": "4" }] }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .update("draft-1", &Map::from_iter([("price".to_owned(), json!(5))]))
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(
        error.source.as_ref().unwrap().details.as_deref().unwrap()["stage"],
        "observe_price"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft", "apply_price"]);
    assert_eq!(recovery.fresh_state.unwrap().values["price"], json!(4));
}
#[tokio::test]
async fn delivery_update_uses_authoritative_state_when_item_update_is_ignored() {
    let transport = FixtureTransport::new([
        response(200, draft("same", json!({ "values": { "title": "Old" } }))),
        response(200, draft("same", json!({ "values": { "title": "Old" } }))),
        response(200, delivery_page(None)),
        response(204, Value::Null),
        response(200, delivery_page(Some("pickup"))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("title".to_owned(), json!("New")),
                ("delivery".to_owned(), json!(["pickup"])),
            ]),
        )
        .await
        .unwrap();

    assert!(!result.etag_changed);
    assert_eq!(result.requested_delivery, ["pickup"]);
    assert_eq!(result.persisted_fields, ["delivery"]);
    assert_eq!(result.ignored_fields, ["title"]);
    assert_eq!(
        result.draft.delivery.unwrap().selected,
        ["pickup".to_owned()]
    );
    let requests = transport.requests();
    let RequestBody::Json(item) = &requests[1].body else {
        panic!("expected item update")
    };
    assert!(item.get("delivery").is_none());
    assert_eq!(requests[3].method, Method::Post);
    assert_eq!(requests[3].path, "/ads/draft-1/delivery");
}
#[tokio::test]
async fn shipping_update_discovers_package_options_and_provider_products() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
        response(200, shipping_page(&["POSTIPAKETTI", "MATKAHUOLTO_SHOP"])),
        response(204, Value::Null),
        response(200, delivery_page(Some("shipping:small"))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["shipping:small"]))]),
        )
        .await
        .unwrap();

    let delivery = result.draft.delivery.unwrap();
    assert_eq!(
        delivery
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        [
            "pickup",
            "shipping:small",
            "shipping:medium",
            "shipping:large"
        ]
    );
    assert_eq!(delivery.selected, ["shipping:small"]);
    let requests = transport.requests();
    assert!(requests[2].path.starts_with("/ui/addelivery/shipping?"));
    let RequestBody::Json(body) = &requests[3].body else {
        panic!("expected delivery body")
    };
    assert_eq!(body["shipping"], true);
    assert_eq!(body["meetup"], false);
    assert_eq!(body["shippingInfo"]["size"], "SMALL");
    assert_eq!(
        body["shippingInfo"]["products"],
        json!(["MATKAHUOLTO_SHOP", "POSTIPAKETTI"])
    );
}
#[tokio::test]
async fn invalid_and_missing_delivery_options_fail_before_mutation() {
    let invalid_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(invalid_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["shipping:oversize"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.invalid_delivery");
    assert_eq!(
        error.details.unwrap()["allowed_values"],
        json!([
            "pickup",
            "shipping:small",
            "shipping:medium",
            "shipping:large"
        ])
    );
    assert_eq!(invalid_transport.requests().len(), 2);

    let empty_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(
            200,
            json!({
                "context": {
                    "adId": "draft-1",
                    "meetup": false,
                    "shipping": false
                },
                "sections": { "deliveryOptions": {} }
            }),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(empty_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.invalid_delivery");
    assert_eq!(error.details.unwrap()["allowed_values"], json!([]));
    assert_eq!(empty_transport.requests().len(), 2);

    let malformed_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, json!({ "context": {}, "sections": {} })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(malformed_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.source.unwrap().details.unwrap()["path"],
        "$.context.adId"
    );
    assert_eq!(malformed_transport.requests().len(), 2);
}
#[tokio::test]
async fn dedicated_delivery_failure_requires_authoritative_recovery() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
        response(503, json!({ "message": "delivery unavailable" })),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    let recovery = error.recovery.unwrap();
    assert_eq!(
        recovery.completed_steps,
        ["fetch_draft", "fetch_delivery_options"]
    );
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_delivery"));
    assert_eq!(recovery.failed_stage.as_deref(), Some("apply_delivery"));
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.delivery, Some(RecoveryStatus::Absent));
    assert_eq!(recovery.absent_fields, ["delivery"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft update draft-1 --delivery VALUE"
        ]
    );
}
#[tokio::test]
async fn discovered_numeric_category_is_sent_as_the_composer_machine_value() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        category_taxonomy("258", true),
        response(200, draft("two", json!({ "values": { "category": 258 } }))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    workflow
        .create(
            Map::from_iter([("category".to_owned(), json!("258"))]),
            &[] as &[&str],
        )
        .await
        .unwrap();

    let requests = transport.requests();
    assert_eq!(requests[1].path, "/categories/taxonomy");
    assert_eq!(
        requests[2].body,
        RequestBody::Json(json!({ "category": 258 }))
    );
}
#[tokio::test]
async fn post_creation_failure_keeps_recovery_context() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        category_taxonomy("furniture/chairs", true),
        response(503, json!({ "message": "category service unavailable" })),
        response(200, draft("one", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .create(
            Map::from_iter([("category".to_owned(), json!("furniture/chairs"))]),
            &[] as &[&str],
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.create_incomplete");
    assert_eq!(
        error.details.as_ref().unwrap()["cause_code"],
        "mutation.uncertain"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(
        recovery.completed_steps,
        ["create_draft", "fetch_category_taxonomy"]
    );
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_category"));
    assert_eq!(recovery.absent_fields, ["category"]);
    let create = recovery.create.unwrap();
    assert_eq!(create.allocation, RecoveryStatus::Persisted);
    assert!(!create.retry_create);
    assert!(create.duplicate_draft_risk);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft update draft-1 --category VALUE"
        ]
    );
}
#[tokio::test]
async fn category_specific_condition_is_source_validated_and_persisted() {
    let option_ids = ["46".to_owned(), "258".to_owned()];
    let mut initial = composer_with_category_options("258", &option_ids);
    let mut category_changed = composer_with_category_options("46", &option_ids);
    let mut condition_changed = category_changed.clone();
    for fixture in [&mut initial, &mut category_changed, &mut condition_changed] {
        fixture["model"]["sections"][2]["content"][1]["exclusive-dependencies"]["category"] =
            json!(["46", "258"]);
    }
    condition_changed["ad"]["values"]["condition"] = json!("2");
    let transport = FixtureTransport::new([
        response(200, initial),
        category_taxonomy("46", true),
        response(200, category_changed),
        response(200, condition_changed),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "46000000",
            &Map::from_iter([
                ("category".to_owned(), json!("46")),
                ("attributes".to_owned(), json!({ "condition": "2" })),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(result.draft.values["condition"], "2");
    assert_eq!(
        result.requested_fields,
        ["attributes.condition", "category"]
    );
    assert!(
        result
            .persisted_fields
            .contains(&"attributes.condition".to_owned())
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    let RequestBody::Json(condition_update) = &requests[3].body else {
        panic!("expected condition composer update")
    };
    assert_eq!(condition_update["condition"], "2");
    assert!(condition_update.get("attributes").is_none());
}
#[tokio::test]
async fn invalid_condition_reports_source_backed_allowed_values_before_mutation() {
    let transport = FixtureTransport::new([response(200, composer_fixture())]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "46000000",
            &Map::from_iter([("attributes".to_owned(), json!({ "condition": "99" }))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let details = error.details.unwrap();
    assert_eq!(details["field_errors"][0]["field"], "attributes.condition");
    assert_eq!(details["field_errors"][0]["code"], "invalid_option");
    let message = details["field_errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("2 (Kuin uusi)"));
    assert_eq!(transport.requests().len(), 1);
}
#[tokio::test]
async fn absent_optional_field_is_distinct_from_an_unsupported_field() {
    let mut fixture = composer_fixture();
    remove_composer_field(&mut fixture, "condition");
    let transport = FixtureTransport::new([response(200, fixture)]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "46000000",
            &Map::from_iter([("attributes".to_owned(), json!({ "condition": "2" }))]),
        )
        .await
        .unwrap_err();

    let details = error.details.unwrap();
    assert_eq!(details["field_errors"][0]["code"], "absent_in_composer");
    assert_eq!(details["field_errors"][0]["source"], "listing_composer");
    assert_eq!(transport.requests().len(), 1);
}
#[tokio::test]
async fn unrecognized_optional_field_type_is_rejected_as_unsupported() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "future_attribute",
            "label": "Future attribute",
            "required": false,
            "type": "future-widget"
        }));
    let transport = FixtureTransport::new([response(200, fixture)]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "46000000",
            &Map::from_iter([(
                "attributes".to_owned(),
                json!({ "future_attribute": "opaque" }),
            )]),
        )
        .await
        .unwrap_err();

    let details = error.details.unwrap();
    assert_eq!(details["field_errors"][0]["code"], "unsupported_by_cli");
    assert_eq!(transport.requests().len(), 1);
}
#[tokio::test]
async fn category_change_revalidates_pending_optional_fields() {
    let option_ids = ["258".to_owned(), "999".to_owned()];
    let initial = composer_with_category_options("258", &option_ids);
    let mut changed = composer_with_category_options("999", &option_ids);
    remove_composer_field(&mut changed, "condition");
    let transport = FixtureTransport::new([
        response(200, initial),
        category_taxonomy("999", true),
        response(200, changed),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "46000000",
            &Map::from_iter([
                ("category".to_owned(), json!("999")),
                ("attributes".to_owned(), json!({ "condition": "2" })),
            ]),
        )
        .await
        .unwrap_err();

    let details = error.details.unwrap();
    assert_eq!(details["field_errors"][0]["code"], "absent_in_composer");
    assert_eq!(transport.requests().len(), 3);
}
#[tokio::test]
async fn generic_optional_composer_field_can_be_changed_and_cleared() {
    let mut initial = composer_fixture();
    initial["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "material",
            "label": "Materiaali",
            "required": false,
            "type": "simple",
            "sub-type": "string"
        }));
    let mut changed = initial.clone();
    changed["ad"]["values"]["material"] = json!("wool");
    let mut cleared = changed.clone();
    cleared["ad"]["values"]
        .as_object_mut()
        .unwrap()
        .remove("material");
    let transport = FixtureTransport::new([
        response(200, initial),
        response(200, changed.clone()),
        response(200, changed),
        response(200, cleared),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let changed = workflow
        .update(
            "46000000",
            &Map::from_iter([("attributes".to_owned(), json!({ "material": "wool" }))]),
        )
        .await
        .unwrap();
    assert_eq!(changed.draft.values["material"], "wool");

    let cleared = workflow
        .update(
            "46000000",
            &Map::from_iter([("attributes".to_owned(), json!({ "material": null }))]),
        )
        .await
        .unwrap();
    assert!(cleared.draft.values.get("material").is_none());
    assert!(
        cleared
            .persisted_fields
            .contains(&"attributes.material".to_owned())
    );
    let requests = transport.requests();
    let RequestBody::Json(clear_update) = &requests[3].body else {
        panic!("expected optional field clear")
    };
    assert!(clear_update["material"].is_null());
}
#[tokio::test]
async fn update_accepts_categories_inside_and_outside_the_compact_composer_page() {
    for requested in ["1", "59"] {
        let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
        let transport = FixtureTransport::new([
            response(200, composer_with_category_options("0", &option_ids)),
            category_taxonomy(requested, true),
            response(200, composer_with_category_options(requested, &option_ids)),
        ]);
        let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

        let updated = workflow
            .update(
                "46000000",
                &Map::from_iter([("category".to_owned(), json!(requested))]),
            )
            .await
            .unwrap();

        assert_eq!(updated.draft.values["category"], json!(requested));
        assert_eq!(transport.requests().len(), 3);
    }
}
#[tokio::test]
async fn category_mutation_errors_distinguish_taxonomy_and_composer_failures() {
    for (requested, taxonomy_id, selectable, expected_code) in [
        ("59", "999", true, "category_not_found"),
        ("59", "59", false, "category_not_selectable"),
        ("60", "60", true, "category_incompatible"),
    ] {
        let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
        let transport = FixtureTransport::new([
            response(200, composer_with_category_options("0", &option_ids)),
            category_taxonomy(taxonomy_id, selectable),
        ]);
        let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

        let error = workflow
            .update(
                "46000000",
                &Map::from_iter([("category".to_owned(), json!(requested))]),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "draft.validation_failed");
        let field_errors = error.details.as_ref().unwrap()["field_errors"]
            .as_array()
            .unwrap();
        assert_eq!(field_errors.len(), 1);
        assert_eq!(field_errors[0]["code"], expected_code);
        let recovery = error.recovery.unwrap();
        assert_eq!(
            recovery.next_safe_actions,
            [
                format!("flea tori category search {requested}"),
                "flea tori draft show 46000000".to_owned(),
            ]
        );
        assert_eq!(transport.requests().len(), 2);
    }
}
