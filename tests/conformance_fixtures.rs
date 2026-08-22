use serde::Deserialize;
use serde_json::Value;
use tori::{
    api::signing::{SigningContext, sign},
    domain::draft::{CategorySchema, DraftValues},
};

#[derive(Deserialize)]
struct SigningVector {
    method: String,
    path_and_query: String,
    service: String,
    body: String,
    signature: String,
}

#[test]
fn gateway_signing_matches_all_fixture_vectors() {
    let vectors: Vec<SigningVector> =
        serde_json::from_str(include_str!("fixtures/signing/vectors.json")).unwrap();

    for vector in vectors {
        let actual = sign(SigningContext {
            method: &vector.method,
            path_and_query: &vector.path_and_query,
            service: &vector.service,
            body: vector.body.as_bytes(),
        });
        assert_eq!(actual.as_header_value(), vector.signature);
    }
}

#[test]
fn normalized_category_fixture_clears_absent_and_invalid_fields() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/domain/category-change.json")).unwrap();
    let mut values: DraftValues = serde_json::from_value(fixture["values"].clone()).unwrap();
    let schema: CategorySchema = serde_json::from_value(fixture["schema"].clone()).unwrap();

    let cleared =
        values.change_category(fixture["replacement_category"].as_str().unwrap(), &schema);

    assert_eq!(
        serde_json::to_value(cleared).unwrap(),
        fixture["cleared_fields"]
    );
    assert_eq!(values.category.as_deref(), Some("furniture/chairs"));
    assert_eq!(values.title.as_deref(), Some("Birch chair"));
    assert_eq!(
        values.attributes,
        [("color".to_owned(), Value::from("red"))].into()
    );
}
