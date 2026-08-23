use super::support::*;
use flea::domain::observation::ObservationSource;

#[tokio::test]
async fn rejects_resource_ids_before_constructing_transport_paths() {
    let transport = FixtureTransport::new([]);
    let api = HttpAdInputApi::new(transport.clone());

    let error = api.get_draft("../credentials").await.unwrap_err();

    assert_eq!(error.code, "draft.invalid_id");
    assert!(transport.requests().is_empty());
}
#[tokio::test]
async fn percent_encodes_upstream_revisions_in_signed_query_targets() {
    let transport = FixtureTransport::new([response(
        200,
        json!({
            "id": "draft-1",
            "choices": [{
                "package-identifier": 10,
                "specification-urn": "urn:product:package-specification:10"
            }]
        }),
    )]);
    let api = HttpAdInputApi::new(transport.clone());

    api.product_context("draft-1", "revision&admin=true")
        .await
        .unwrap();

    assert_eq!(
        transport.requests()[0].path,
        "/adinput/product/recommerce/draft-1/productcontext?adRevision=revision%26admin%3Dtrue"
    );
}
#[test]
fn request_debug_redacts_targets_raw_bytes_and_secret_json_values() {
    let image = HttpRequest {
        method: Method::Post,
        path: "/images".to_owned(),
        observation_source: ObservationSource::DraftService,
        service: None,
        if_match: None,
        retry: RetryPolicy::Never,
        body: RequestBody::Image {
            bytes: b"raw-image-secret".to_vec(),
            file_name: "image.jpg".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            width: 1,
            height: 1,
        },
    };
    let json = HttpRequest {
        method: Method::Post,
        path: "/drafts".to_owned(),
        observation_source: ObservationSource::DraftService,
        service: None,
        if_match: None,
        retry: RetryPolicy::Never,
        body: RequestBody::Json(json!({ "access_token": "token-secret" })),
    };
    let delivery = HttpRequest {
        method: Method::Get,
        path: "/ui/addelivery/shipping?name=Private+Seller".to_owned(),
        observation_source: ObservationSource::DeliveryComposer,
        service: None,
        if_match: None,
        retry: RetryPolicy::BoundedRead,
        body: RequestBody::Empty,
    };

    assert!(!format!("{image:?}").contains("raw-image-secret"));
    assert!(!format!("{json:?}").contains("token-secret"));
    assert!(!format!("{delivery:?}").contains("Private"));
}
