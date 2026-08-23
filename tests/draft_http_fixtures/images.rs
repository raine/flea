use super::support::*;

#[tokio::test]
async fn image_upload_reads_dimensions_and_preserves_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("second.png");
    image::DynamicImage::new_rgb8(7, 11).save(&path).unwrap();
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"private-gps-metadata")
        .unwrap();
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [image("first", 0, "ready", 4, 6)]
                }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.png"),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "processing", 7, 11)
                    ]
                }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "processing", 7, 11)
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.add_images("draft-1", &[path]).await.unwrap();

    assert_eq!(state.revision.as_deref(), Some("three"));
    assert_eq!(state.images[1].state, ImageState::Processing);
    assert_eq!(state.image_processing.len(), 1);
    assert_eq!(state.image_processing[0].source_format, "png");
    assert_eq!(state.image_processing[0].uploaded_format, "png");
    assert_eq!(state.image_processing[0].final_width, 7);
    assert_eq!(state.image_processing[0].final_height, 11);
    assert!(state.image_processing[0].metadata_stripped);
    assert!(state.image_processing[0].recompressed);
    let requests = transport.requests();
    let RequestBody::Image {
        bytes,
        file_name,
        mime_type,
        width,
        height,
    } = &requests[1].body
    else {
        panic!("expected image upload")
    };
    assert_eq!((*width, *height), (7, 11));
    assert_eq!(file_name, "image.png");
    assert_eq!(mime_type, "image/png");
    assert!(
        !bytes
            .windows(b"private-gps-metadata".len())
            .any(|window| window == b"private-gps-metadata")
    );
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(requests[1].path, "/adinput/ad/recommerce/draft-1/upload");
    let RequestBody::Json(values) = &requests[2].body else {
        panic!("expected image field update")
    };
    assert_eq!(values["multi_image"].as_array().unwrap().len(), 2);
    assert_eq!(values["multi_image"][1]["width"], 7);
    assert_eq!(values["multi_image"][1]["height"], 11);
}
#[tokio::test]
async fn post_attachment_inspection_accepts_a_stale_composer_endpoint_model() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.png");
    image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
    let image_url = "https://img.tori.net/dynamic/default/image.jpg";
    let transport = FixtureTransport::new([
        response(200, draft("before-images", json!({}))),
        response_with_location(201, image_url),
        response(
            200,
            draft(
                "attachment-response",
                json!({ "images": [image("image", 0, "ready", 4, 6)] }),
            ),
        ),
        response(
            200,
            json!({
                "ad": {
                    "id": "draft-1",
                    "etag": "post-image",
                    "values": {
                        "multi_image": [{
                            "description": "",
                            "height": 6,
                            "path": "image.jpg",
                            "type": "image/jpeg",
                            "url": image_url,
                            "width": 4
                        }]
                    }
                }
            }),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let result = workflow.add_images("draft-1", &[path]).await.unwrap();

    assert_eq!(result.revision.as_deref(), Some("post-image"));
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].image_id, image_url);
}
#[tokio::test]
async fn post_attachment_conflict_preserves_revision_values_and_uploaded_image_ids() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..2)
        .map(|index| {
            let path = directory.path().join(format!("image-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let attached = draft(
        "1179389950",
        json!({
            "values": {
                "title": "Chair",
                "description": "Solid birch chair",
                "trade_type": "sell",
                "price": 45,
                "postal_code": "00100"
            },
            "images": [
                image("first", 0, "ready", 4, 6),
                image("second", 1, "ready", 4, 6)
            ]
        }),
    );
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "before-images",
                json!({ "values": attached["values"].clone() }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/first.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.jpg"),
        response(200, attached),
        response(404, json!({ "message": "missing" })),
    ])
    .with_search_responses([listing_collection("draft-1", "DRAFT")]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.add_images("draft-1", &paths).await.unwrap_err();

    assert_eq!(error.code, "draft.observation_conflict");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.observed_revision.as_deref(), Some("1179389950"));
    assert_eq!(recovery.requested_values["title"], "Chair");
    assert_eq!(recovery.requested_values["price"], 45);
    assert_eq!(recovery.images.len(), 2);
    assert!(recovery.images.iter().all(|image| {
        image.status == RecoveryStatus::Persisted
            && image.upload == UploadRecoveryStatus::Completed
            && image.attachment == AttachmentRecoveryStatus::Attached
            && image.image_id.is_some()
    }));
    assert_eq!(
        recovery.observation.state,
        ObservationState::ConflictingSources
    );
    assert_eq!(
        recovery.next_safe_actions,
        ["flea tori draft show draft-1", "flea tori listing list"]
    );
    assert!(recovery.destructive_actions.is_empty());
}
#[tokio::test]
async fn image_failure_recovery_uses_absolute_positions_and_lifecycle_states() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("private-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [
                        image("existing-a", 0, "ready", 4, 6),
                        image("existing-b", 1, "ready", 4, 6)
                    ]
                }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/new-a.jpg"),
        response(422, json!({ "message": "image rejected" })),
        response(
            200,
            draft(
                "observed",
                json!({
                    "images": [
                        image("existing-a", 0, "ready", 4, 6),
                        image("existing-b", 1, "ready", 4, 6)
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.add_images("draft-1", &paths).await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.failed_stage.as_deref(), Some("upload_image:3"));
    assert_eq!(recovery.completed_steps, ["fetch_draft", "upload_image:2"]);
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.observed_etag.as_deref(), Some("observed"));
    let image = |index| {
        recovery
            .images
            .iter()
            .find(|image| image.index == index)
            .unwrap()
    };
    assert_eq!(image(2).status, RecoveryStatus::Absent);
    assert_eq!(image(2).upload, UploadRecoveryStatus::Completed);
    assert_eq!(image(2).attachment, AttachmentRecoveryStatus::Absent);
    assert_eq!(image(3).status, RecoveryStatus::Rejected);
    assert_eq!(image(3).upload, UploadRecoveryStatus::Failed);
    assert_eq!(image(4).status, RecoveryStatus::Unattempted);
    assert_eq!(image(4).upload, UploadRecoveryStatus::Unattempted);
    assert!(
        recovery
            .next_safe_actions
            .iter()
            .all(|action| !action.contains("image add"))
    );
    assert_eq!(
        recovery.destructive_actions,
        ["flea tori draft delete draft-1"]
    );
}
#[tokio::test]
async fn missing_ad_attachment_success_reconciles_multiple_ready_images() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..2)
        .map(|index| {
            let path = directory.path().join(format!("private-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response_with_location(201, "https://img.tori.net/dynamic/default/first.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.jpg"),
        response(200, json!({ "model": { "sections": [] } })),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "ready", 4, 6)
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow.add_images("draft-1", &paths).await.unwrap();

    assert_eq!(result.images.len(), 2);
    assert!(
        result
            .images
            .iter()
            .all(|image| image.state == ImageState::Ready)
    );
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("authoritative observation"));
    let requests = transport.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == Method::Put)
            .count(),
        1
    );
}
#[tokio::test]
async fn attachment_recovery_distinguishes_processing_ready_and_failed_images() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("private-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let mut uncertain = response(200, Value::Null);
    uncertain.body_is_unparseable = true;
    let mut failed = image("third", 2, "failed", 4, 6);
    failed["failure"] = json!("processing rejected");
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response_with_location(201, "https://img.tori.net/dynamic/default/first.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/third.jpg"),
        uncertain,
        response(
            200,
            draft(
                "observed",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "processing", 4, 6),
                        failed
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.add_images("draft-1", &paths).await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.failed_stage.as_deref(), Some("attach_images"));
    let image = |index| {
        recovery
            .images
            .iter()
            .find(|image| image.index == index)
            .unwrap()
    };
    assert_eq!(image(0).status, RecoveryStatus::Persisted);
    assert_eq!(image(0).processing, ProcessingRecoveryStatus::Ready);
    assert_eq!(image(1).status, RecoveryStatus::Pending);
    assert_eq!(image(1).processing, ProcessingRecoveryStatus::Processing);
    assert_eq!(image(2).status, RecoveryStatus::Rejected);
    assert_eq!(image(2).processing, ProcessingRecoveryStatus::Failed);
}
#[tokio::test]
async fn uncertain_image_removal_reports_only_retained_images_as_safe_work() {
    let first = "https://img.tori.net/dynamic/default/first.jpg".to_owned();
    let second = "https://img.tori.net/dynamic/default/second.jpg".to_owned();
    let observed = json!({
        "images": [
            image("first", 0, "ready", 4, 6),
            image("second", 1, "ready", 4, 6)
        ]
    });
    let mut uncertain = response(200, Value::Null);
    uncertain.body_is_unparseable = true;
    let transport = FixtureTransport::new([
        response(200, draft("one", observed.clone())),
        uncertain,
        response(200, draft("observed", observed)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .remove_images("draft-1", &[first, second])
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert!(recovery.images.iter().all(|image| {
        image.operation == ImageRecoveryOperation::Remove
            && image.status == RecoveryStatus::Absent
            && image.attachment == AttachmentRecoveryStatus::Attached
    }));
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea tori draft show draft-1",
            "flea tori draft image remove draft-1 IMAGE_ID..."
        ]
    );
}
#[tokio::test]
async fn image_removal_accepts_authoritative_success_after_unrecognized_response() {
    let removed = "https://img.tori.net/dynamic/default/removed.jpg".to_owned();
    let mut uncertain = response(200, Value::Null);
    uncertain.body_is_unparseable = true;
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "images": [image("removed", 0, "ready", 4, 6)] }),
            ),
        ),
        uncertain,
        response(200, draft("observed", json!({ "images": [] }))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let state = workflow.remove_images("draft-1", &[removed]).await.unwrap();

    assert!(state.images.is_empty());
    assert_eq!(state.etag, "observed");
}
#[tokio::test]
async fn remove_and_delete_use_ordered_non_retried_mutations() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [
                        image("third", 2, "ready", 3, 3),
                        image("first", 0, "ready", 1, 1),
                        image("second", 1, "ready", 2, 2)
                    ]
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        image("first", 0, "ready", 1, 1),
                        image("third", 1, "ready", 3, 3)
                    ]
                }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({
                    "images": [
                        image("first", 0, "ready", 1, 1),
                        image("third", 1, "ready", 3, 3)
                    ]
                }),
            ),
        ),
        response(200, draft("pre-delete", json!({}))),
        response(204, Value::Null),
    ])
    .with_search_responses([response(
        200,
        json!({
            "summaries": [{
                "id": "draft-1",
                "actions": [{
                    "method": "DELETE",
                    "name": "DELETE",
                    "path": "/items/draft-1"
                }]
            }],
            "total": 1
        }),
    )]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .remove_images(
            "draft-1",
            &["https://img.tori.net/dynamic/default/second.jpg".to_owned()],
        )
        .await
        .unwrap();
    workflow.delete("draft-1").await.unwrap();

    assert!(state.images[1].image_id.ends_with("third.jpg"));
    let requests = transport.requests();
    let RequestBody::Json(values) = &requests[1].body else {
        panic!("expected image field update")
    };
    assert_eq!(values["multi_image"].as_array().unwrap().len(), 2);
    assert!(
        values["multi_image"][1]["url"]
            .as_str()
            .unwrap()
            .ends_with("third.jpg")
    );
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(requests[2].method, Method::Get);
    assert_eq!(requests[2].retry, RetryPolicy::BoundedRead);
    assert_eq!(requests[3].method, Method::Get);
    assert_eq!(requests[3].path, "/adinput/ad/withModel/draft-1");
    assert_eq!(requests[4].method, Method::Get);
    assert_eq!(requests[4].path, "/search?facet=DRAFT&limit=50&offset=0");
    assert_eq!(
        requests[4].service,
        Some(flea::marketplace::tori::client::compatibility::SERVICE_AD_SUMMARIES)
    );
    assert_eq!(requests[5].method, Method::Delete);
    assert_eq!(requests[5].path, "/items/draft-1");
    assert_eq!(requests[5].retry, RetryPolicy::Never);
    assert_eq!(
        requests[5].service,
        Some(flea::marketplace::tori::client::compatibility::SERVICE_AD_ACTION)
    );
}
