use super::support::*;

#[tokio::test]
async fn incomplete_creation_continues_with_update_and_image_actions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("continuation.png");
    image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(
            422,
            json!({
                "errors": [{
                    "field": "title",
                    "code": "invalid",
                    "message": "title was rejected"
                }]
            }),
        ),
        response(200, draft("one", json!({}))),
        response(200, draft("two", json!({ "values": { "title": "Chair" } }))),
        response(
            200,
            draft(
                "three",
                json!({ "values": { "title": "Chair", "postal_code": "00100" } }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({ "values": { "title": "Chair", "postal_code": "00100" } }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/continued.png"),
        response(
            200,
            draft(
                "four",
                json!({
                    "values": { "title": "Chair", "postal_code": "00100" },
                    "images": [image("continued", 0, "ready", 4, 6)]
                }),
            ),
        ),
        response(
            200,
            draft(
                "five",
                json!({
                    "values": { "title": "Chair", "postal_code": "00100" },
                    "images": [image("continued", 0, "ready", 4, 6)]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());
    let requested = Map::from_iter([
        ("title".to_owned(), json!("Chair")),
        ("postal_code".to_owned(), json!("00100")),
    ]);

    let error = workflow
        .create(requested.clone(), &[&path])
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.create_incomplete");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["title"]);
    assert_eq!(recovery.unattempted_fields, ["postal_code"]);
    assert!(recovery.next_safe_actions.iter().any(|action| action
        == "flea tori draft update draft-1 --input PATH_WITH_ABSENT_AND_UNATTEMPTED_FIELDS"));
    assert!(
        recovery
            .next_safe_actions
            .iter()
            .any(|action| action == "flea tori draft image add draft-1 --image PATH...")
    );

    let updated = workflow.update("draft-1", &requested).await.unwrap();
    assert_eq!(updated.persisted_fields, ["postal_code", "title"]);
    let continued = workflow.add_images("draft-1", &[&path]).await.unwrap();
    assert_eq!(continued.images.len(), 1);
}
#[tokio::test]
async fn image_failure_keeps_allocated_draft_and_unattempted_image_plan() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");
    image::DynamicImage::new_rgb8(4, 6).save(&first).unwrap();
    image::DynamicImage::new_rgb8(4, 6).save(&second).unwrap();
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(503, json!({ "message": "upload unavailable" })),
        response(200, draft("one", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .create(Map::new(), &[&first, &second])
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.create_incomplete");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.images[0].status, RecoveryStatus::Rejected);
    assert_eq!(recovery.images[1].status, RecoveryStatus::Unattempted);
    assert!(
        recovery
            .next_safe_actions
            .iter()
            .any(|action| action == "flea tori draft image add draft-1 --image PATH...")
    );
}
#[tokio::test]
async fn recovery_output_bounds_dynamic_fields_images_steps_and_local_paths() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private-recovery-image.png");
    image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
    let paths = vec![path; 25];
    let mut values = Map::from_iter([("category".to_owned(), json!("chairs"))]);
    for index in 0..30 {
        values.insert(
            format!("dynamic_field_{index:02}"),
            json!(format!("private-value-{index}")),
        );
    }
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        category_taxonomy("chairs", true),
        response(503, json!({ "message": "category unavailable" })),
        response(200, draft("observed", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.create(values, &paths).await.unwrap_err();

    let debug = format!("{error:?}");
    let recovery = error.recovery.unwrap();
    assert!(recovery.field_summary.len() <= 24);
    assert!(recovery.fields_omitted > 0);
    assert_eq!(recovery.images.len(), 20);
    assert_eq!(recovery.images_omitted, 5);
    assert!(recovery.completed_steps.len() <= 24);
    let rendered = serde_json::to_string(&recovery).unwrap();
    assert!(!rendered.contains("private-value"));
    assert!(!rendered.contains("private-recovery-image"));
    assert!(!debug.contains("private-value"));
    assert!(!debug.contains("private-recovery-image"));
}
#[tokio::test]
async fn owned_active_listing_creates_a_fresh_inspectable_draft() {
    let transport = FixtureTransport::new([
        response(
            200,
            json!({ "listing_id": "listing-7", "values": {}, "images": [] }),
        ),
        response(201, draft("one", json!({}))),
    ])
    .with_search_responses([listing_collection("listing-7", "ACTIVE")]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow.create_from_listing("listing-7").await.unwrap();

    assert_eq!(result.draft.draft_id, "draft-1");
    assert_eq!(
        result.completed_steps,
        ["load_source_listing", "create_draft"]
    );
    let copy = result.listing_copy.unwrap();
    assert_eq!(copy.source_scope, "authenticated_seller_listings");
    assert!(copy.copied_fields.is_empty());
    assert_eq!(copy.image_handling, "fresh_upload_from_source_bytes");
    assert_eq!(
        transport
            .requests()
            .iter()
            .map(|request| (request.method.clone(), request.path.as_str()))
            .collect::<Vec<_>>(),
        [
            (Method::Get, "/search?limit=50&offset=0"),
            (Method::Get, "/listings/listing-7/draft-source"),
            (Method::Post, "/adinput/ad/withModel/recommerce"),
        ]
    );
}
#[tokio::test]
async fn owned_expired_listing_remains_in_the_supported_source_scope() {
    let transport = FixtureTransport::new([
        response(
            200,
            json!({ "listing_id": "listing-7", "values": {}, "images": [] }),
        ),
        response(201, draft("one", json!({}))),
    ])
    .with_search_responses([listing_collection("listing-7", "EXPIRED")]);

    let result = DraftWorkflow::new(HttpAdInputApi::new(transport), config())
        .create_from_listing("listing-7")
        .await
        .unwrap();

    assert_eq!(result.draft.draft_id, "draft-1");
}
#[tokio::test]
async fn third_party_public_listing_is_rejected_before_draft_allocation() {
    let transport = FixtureTransport::new([]);

    let error = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
        .create_from_listing("listing-7")
        .await
        .unwrap_err();

    assert_eq!(error.code, "listing.not_copyable");
    let source = error.source.unwrap();
    assert_eq!(
        source.observation.unwrap().source,
        "listing_copy_eligibility"
    );
    assert_eq!(
        source.details.unwrap()["remote_draft_allocated"],
        json!(false)
    );
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}
#[tokio::test]
async fn deleted_listing_is_rejected_before_draft_allocation() {
    let transport = FixtureTransport::new([])
        .with_search_responses([response(200, json!({ "summaries": [], "total": 0 }))]);

    let error = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
        .create_from_listing("deleted-7")
        .await
        .unwrap_err();

    assert_eq!(error.code, "listing.not_copyable");
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].path, "/search?limit=50&offset=0");
}
#[tokio::test]
async fn unavailable_copy_source_preserves_known_listing_presence_without_allocating_a_draft() {
    for (status, expected_code) in [
        (404, "listing.not_copyable"),
        (503, "upstream.request_failed"),
    ] {
        let transport =
            FixtureTransport::new([response(status, json!({ "message": "source unavailable" }))])
                .with_search_responses([listing_collection("listing-7", "ACTIVE")]);

        let error = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
            .create_from_listing("listing-7")
            .await
            .unwrap_err();

        assert_eq!(error.code, expected_code);
        let source = error.source.unwrap();
        assert_eq!(
            source.observation.as_ref().unwrap().source,
            "listing_copy_eligibility"
        );
        let details = source.details.unwrap();
        assert_eq!(details["listing_presence"]["state"], "confirmed_present");
        assert_eq!(details["remote_draft_allocated"], false);
        assert!(
            transport
                .requests()
                .iter()
                .all(|request| request.method == Method::Get)
        );
    }
}
#[tokio::test]
async fn copied_images_are_preprocessed_and_uploaded_as_fresh_attachments() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.png");
    image::DynamicImage::new_rgb8(7, 11)
        .save(&source_path)
        .unwrap();
    let source_bytes = std::fs::read(source_path).unwrap();
    let transport = FixtureTransport::new([
        response(
            200,
            json!({
                "listing_id": "listing-7",
                "values": {
                    "seller_id": "private-seller",
                    "multi_image": ["https://img.example/published.jpg"]
                },
                "images": [{ "file_name": "published.png", "bytes": source_bytes }]
            }),
        ),
        response(201, draft("one", json!({}))),
        response_with_location(201, "https://img.tori.net/dynamic/default/fresh-image.jpg"),
        response(
            200,
            draft(
                "two",
                json!({ "images": [image("fresh-image", 0, "processing", 7, 11)] }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({ "images": [image("fresh-image", 0, "processing", 7, 11)] }),
            ),
        ),
    ])
    .with_search_responses([listing_collection("listing-7", "ACTIVE")]);

    let result = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
        .create_from_listing("listing-7")
        .await
        .unwrap();

    let copy = result.listing_copy.unwrap();
    assert_eq!(copy.source_image_count, 1);
    assert_eq!(copy.omitted_fields, ["multi_image", "seller_id"]);
    assert!(copy.copied_fields.is_empty());
    let requests = transport.requests();
    assert!(
        requests
            .iter()
            .any(|request| { request.method == Method::Post && request.path.ends_with("/upload") })
    );
    let attachment = requests
        .iter()
        .find(|request| request.method == Method::Put)
        .unwrap();
    let RequestBody::Json(body) = &attachment.body else {
        panic!("image attachment must use JSON")
    };
    let encoded = serde_json::to_string(body).unwrap();
    assert!(encoded.contains("fresh-image.jpg"));
    assert!(!encoded.contains("published.jpg"));
    assert!(!encoded.contains("private-seller"));
}
#[tokio::test]
async fn copy_failure_identifies_both_source_listing_and_created_draft() {
    let transport = FixtureTransport::new([
        response(
            200,
            json!({ "listing_id": "listing-7", "values": { "title": "Chair" }, "images": [] }),
        ),
        response(201, draft("one", json!({}))),
        response(503, json!({ "message": "copy failed" })),
        response(200, draft("one", json!({}))),
    ])
    .with_search_responses([listing_collection("listing-7", "ACTIVE")]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.create_from_listing("listing-7").await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.source_listing_id.as_deref(), Some("listing-7"));
    assert_eq!(recovery.listing_id, None);
    assert_eq!(
        recovery.completed_steps,
        ["load_source_listing", "create_draft"]
    );
}
