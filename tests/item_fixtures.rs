use std::{future::Future, pin::Pin, sync::Mutex};

use clap::Parser;
use flea::{
    cli::{Cli, Command, ToriCommand, item},
    marketplace::tori::item::{PublicItemApi, PublicItemApiError, PublicItems},
};
use serde_json::{Value, json};

struct FixtureApi {
    response: Result<Value, PublicItemApiError>,
    ids: Mutex<Vec<String>>,
}

impl FixtureApi {
    fn success(response: Value) -> Self {
        Self {
            response: Ok(response),
            ids: Mutex::default(),
        }
    }

    fn error(error: PublicItemApiError) -> Self {
        Self {
            response: Err(error),
            ids: Mutex::default(),
        }
    }
}

impl PublicItemApi for FixtureApi {
    fn item<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, PublicItemApiError>> + Send + 'a>> {
        self.ids.lock().unwrap().push(listing_id.to_owned());
        let response = self.response.clone();
        Box::pin(async move { response })
    }
}

#[tokio::test]
async fn normalizes_complete_public_listing_detail() {
    let api = FixtureApi::success(full_fixture());
    let (item, _) = PublicItems::new(&api).show("42346404").await.unwrap();

    assert_eq!(item.listing_id, "42346404");
    assert_eq!(item.title, "Potkulauta");
    assert!(item.description.contains("Micro Mini"));
    assert_eq!(item.trade_type, flea::domain::commerce::TradeType::Sell);
    assert_eq!(item.price.kind, flea::domain::commerce::PriceKind::Fixed);
    assert_eq!(item.price.amount, Some(json!(25)));
    assert_eq!(item.price.currency.as_deref(), Some("EUR"));
    assert_eq!(
        item.location.as_ref().unwrap().name.as_deref(),
        Some("Helsinki, Uusimaa")
    );
    assert_eq!(item.condition.as_ref().unwrap().value, "Hyvä");
    assert_eq!(item.seller.seller_type.as_deref(), Some("private"));
    assert_eq!(item.seller.display_name.as_deref(), Some("Maija"));
    assert_eq!(item.shipping.available, Some(true));
    assert_eq!(item.shipping.seller_pays, Some(false));
    assert_eq!(item.images.len(), 2);
    assert_eq!(
        item.published_at.as_deref(),
        Some("2026-08-20T08:00:00+03:00")
    );
    assert_eq!(item.published_at_ms, Some(1_787_200_000_000));
    assert_eq!(
        item.canonical_url.as_deref(),
        Some("https://www.tori.fi/recommerce/forsale/item/42346404")
    );
    assert_eq!(item.category[0].value, "Lasten tarvikkeet");
    assert_eq!(item.attributes[1].value, "Micro");

    let serialized = serde_json::to_string(&item).unwrap();
    for private in ["ownerId", "lat", "lng", "ownerUrn"] {
        assert!(
            !serialized.contains(private),
            "normalized output leaked {private}"
        );
    }
}

#[tokio::test]
async fn raw_mode_preserves_the_exact_upstream_document() {
    let raw = full_fixture();
    let api = FixtureApi::success(raw.clone());
    let cli = Cli::parse_from(["flea", "tori", "item", "show", "42346404", "--raw"]);
    let Command::Tori(tori) = cli.command else {
        unreachable!()
    };
    let ToriCommand::Item(args) = tori.command else {
        unreachable!()
    };

    let output = item::dispatch(args, &api).await.unwrap();
    assert_eq!(serde_json::to_value(output.data).unwrap(), raw);
}

#[tokio::test]
async fn invalid_ids_fail_locally_with_an_actionable_structured_error() {
    let api = FixtureApi::success(full_fixture());
    let error = PublicItems::new(&api)
        .show("../42346404")
        .await
        .unwrap_err();

    assert_eq!(error.code, "item.invalid_id");
    assert_eq!(error.next_actions[0].command, "flea tori search");
    assert!(api.ids.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_expired_and_upstream_invalid_items_have_distinct_actionable_errors() {
    for (api_error, code) in [
        (PublicItemApiError::NotFound, "item.not_found"),
        (PublicItemApiError::Expired, "item.expired"),
        (PublicItemApiError::Invalid, "item.invalid_id"),
    ] {
        let error = PublicItems::new(&FixtureApi::error(api_error))
            .show("42346404")
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.next_actions[0].command, "flea tori search");
        assert_eq!(error.details.as_deref().unwrap()["listing_id"], "42346404");
    }
}

#[tokio::test]
async fn read_failures_separate_transience_from_safe_replay_and_redact_details() {
    for (api_error, upstream_transient) in [
        (
            PublicItemApiError::Unexpected("secret response body".to_owned().into()),
            false,
        ),
        (
            PublicItemApiError::Transport("secret request target".to_owned().into()),
            true,
        ),
        (PublicItemApiError::Upstream(503), true),
        (PublicItemApiError::Upstream(403), false),
    ] {
        let error = PublicItems::new(&FixtureApi::error(api_error))
            .show("42346404")
            .await
            .unwrap_err();
        assert_eq!(error.upstream_transient, upstream_transient);
        assert!(error.safe_to_retry);
        assert!(!format!("{error:?}").contains("secret"));
    }
}

#[tokio::test]
async fn sparse_upstream_details_keep_required_normalized_sections_explicit() {
    let api = FixtureApi::success(json!({
        "ad": {"title": "Free item", "description": ""},
        "meta": {"adId": "1"}
    }));
    let (item, _) = PublicItems::new(&api).show("1").await.unwrap();
    let value = serde_json::to_value(item).unwrap();

    assert!(value.get("seller").unwrap().is_object());
    assert!(value.get("shipping").unwrap().is_object());
    assert!(value.get("images").unwrap().is_array());
    assert_eq!(value["trade_type"], "unknown");
    assert_eq!(value["price"]["kind"], "unavailable");
    assert!(value["price"].get("amount").is_none());
    assert!(value.get("published_at").unwrap().is_null());
    assert!(value.get("canonical_url").unwrap().is_null());
}

fn full_fixture() -> Value {
    json!({
        "ad": {
            "price": {"amount": 25, "currencyCode": "EUR", "display": "25 €"},
            "title": "Potkulauta",
            "description": "Hyväkuntoinen Micro Mini lasten potkulauta.",
            "extras": [
                {"id": "condition", "label": "Kunto", "value": "Hyvä", "valueId": 3},
                {"id": "brand", "label": "Merkki", "value": "Micro", "valueId": 42}
            ],
            "images": [
                {"uri": "https://img.tori.net/item/one", "width": 1200, "height": 800, "description": "Side view"},
                {"url": "https://img.tori.net/item/two"}
            ],
            "category": {
                "id": 20,
                "value": "Potkulaudat",
                "parent": {"id": 10, "value": "Lasten tarvikkeet"}
            },
            "location": {
                "postalName": "Helsinki, Uusimaa",
                "postalCode": "00100",
                "countryCode": "FI",
                "position": {"lat": 60.17, "lng": 24.94}
            },
            "condition": {"id": 3, "value": "Hyvä"},
            "adViewTypeLabel": "Myydään",
            "timestamp": 1787200000000_i64
        },
        "meta": {
            "adId": 42346404,
            "ownerId": 123456,
            "ownerUrn": "secret-owner",
            "history": [
                {"mode": "PAUSE", "broadcasted": "2026-08-21T09:00:00+03:00"},
                {"mode": "PLAY", "broadcasted": "2026-08-20T08:00:00+03:00"}
            ]
        },
        "seller": {
            "type": "private",
            "displayName": "Maija",
            "profileUrl": "https://www.tori.fi/profile/maija",
            "verified": true
        },
        "transactableData": {
            "transactable": true,
            "eligibleForShipping": true,
            "sellerPaysShipping": false,
            "buyNow": true,
            "method": "ToriDiili",
            "price": 2.95
        },
        "canonical_url": "https://www.tori.fi/recommerce/forsale/item/42346404"
    })
}
