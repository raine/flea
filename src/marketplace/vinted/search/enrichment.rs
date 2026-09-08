use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use super::{DecimalAmount, VintedSearchApi};
use crate::{
    domain::search::SearchCollection, error::AppError,
    marketplace::vinted::auth::VintedCredentialRecord,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub seller_country: Option<String>,
    pub shipping_to: Option<DecimalAmount>,
    pub include_seller: bool,
    pub include_shipping: bool,
}

pub fn validate(options: &Options) -> Result<(), AppError> {
    if options.seller_country.as_deref().is_some_and(|country| {
        !["FI", "Finland", "Suomi"]
            .iter()
            .any(|supported| country.eq_ignore_ascii_case(supported))
    }) {
        return Err(AppError::usage(
            "Vinted seller country supports only FI, Finland, or Suomi",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Limits {
    items: usize,
    request: Duration,
    total: Duration,
}

const LIMITS: Limits = Limits {
    items: 20,
    request: Duration::from_secs(3),
    total: Duration::from_secs(30),
};

pub async fn enrich(
    api: &dyn VintedSearchApi,
    credentials: &VintedCredentialRecord,
    collection: &mut SearchCollection,
    options: &Options,
) {
    enrich_with_limits(api, credentials, collection, options, LIMITS).await;
}

async fn enrich_with_limits(
    api: &dyn VintedSearchApi,
    credentials: &VintedCredentialRecord,
    collection: &mut SearchCollection,
    options: &Options,
    limits: Limits,
) {
    let seller_requested = options.include_seller || options.seller_country.is_some();
    let shipping_requested = options.include_shipping || options.shipping_to.is_some();
    if !seller_requested && !shipping_requested {
        return;
    }

    let deadline = Instant::now() + limits.total;
    let original_count = collection.results.len();
    let mut statuses = Vec::with_capacity(original_count);
    let mut requests = 0;
    let mut unknown = 0;
    let mut errors = 0;
    let mut excluded = 0;
    let mut unprocessed = 0;
    let mut retained = Vec::with_capacity(original_count);
    for (index, mut listing) in std::mem::take(&mut collection.results)
        .into_iter()
        .enumerate()
    {
        let mut stages = Vec::with_capacity(2);
        for (requested, shipping) in [(seller_requested, false), (shipping_requested, true)] {
            let stage = if !requested {
                status("not_requested", None)
            } else if index >= limits.items {
                status("unprocessed", Some("item_limit"))
            } else if Instant::now() >= deadline {
                status("unprocessed", Some("total_deadline"))
            } else {
                requests += 1;
                fetch(
                    api,
                    credentials,
                    &listing.listing_id,
                    shipping,
                    deadline,
                    limits.request,
                )
                .await
            };
            stages.push(stage);
        }
        let seller = &stages[0];
        let shipping = &stages[1];
        let country_filter = country_filter(seller, options.seller_country.is_some());
        let postage_filter = shipping_filter(shipping, options.shipping_to.as_ref());
        let keep = [&country_filter, &postage_filter]
            .iter()
            .all(|filter| matches!(filter["status"].as_str(), Some("matched" | "not_requested")));
        unknown += usize::from(stages.iter().any(|stage| stage["status"] == "unknown"));
        errors += usize::from(stages.iter().any(|stage| stage["status"] == "error"));
        unprocessed += usize::from(stages.iter().any(|stage| stage["status"] == "unprocessed"));
        excluded += usize::from(!keep);
        statuses.push(json!({
            "listing_id": listing.listing_id,
            "seller": diagnostic_status(seller),
            "shipping": diagnostic_status(shipping),
            "filters": {"seller_country": country_filter, "shipping_to": postage_filter},
            "included": keep,
        }));
        let vinted = listing.vinted.get_or_insert_with(|| json!({}));
        if !vinted.is_object() {
            *vinted = json!({});
        }
        if seller_requested {
            vinted["seller"] = seller.clone();
        }
        if shipping_requested {
            vinted["shipping"] = shipping.clone();
        }
        if keep {
            retained.push(listing);
        }
    }
    collection.results = retained;
    collection.pagination.returned = collection.results.len();
    collection.enrichment = Some(json!({
        "scope": "upstream_page",
        "considered": original_count,
        "returned": collection.results.len(),
        "requests": requests,
        "unknown": unknown,
        "errors": errors,
        "excluded": excluded,
        "unprocessed": unprocessed,
        "coverage": {
            "complete": unknown == 0 && errors == 0 && unprocessed == 0,
            "fully_processed_items": original_count - unprocessed,
        },
        "limits": {
            "items": limits.items,
            "concurrency": 1,
            "request_timeout_ms": limits.request.as_millis() as u64,
            "total_timeout_ms": limits.total.as_millis() as u64,
        },
        "filters": {
            "seller_country": options.seller_country.as_ref().map(|_| "FI"),
            "shipping_to": options.shipping_to.as_ref().map(ToString::to_string),
            "currency": "EUR",
        },
        "items": statuses,
    }));
}

async fn fetch(
    api: &dyn VintedSearchApi,
    credentials: &VintedCredentialRecord,
    listing_id: &str,
    shipping: bool,
    deadline: Instant,
    request_timeout: Duration,
) -> Value {
    let request_deadline = (Instant::now() + request_timeout).min(deadline);
    match timeout_at(
        request_deadline,
        api.enrichment(credentials, listing_id, shipping),
    )
    .await
    {
        Ok(Ok(raw)) if shipping => normalize_shipping(&raw),
        Ok(Ok(raw)) => normalize_seller(&raw, listing_id),
        Ok(Err(error)) => json!({
            "status": "error",
            "code": error.code,
            "upstream_transient": error.upstream_transient,
            "safe_to_retry": error.safe_to_retry,
        }),
        Err(_) => json!({
            "status": "error",
            "code": "vinted_search.enrichment_timeout",
            "reason": if request_deadline == deadline { "total_deadline" } else { "request_timeout" },
            "upstream_transient": true,
            "safe_to_retry": true,
        }),
    }
}

fn diagnostic_status(enrichment: &Value) -> Value {
    let mut diagnostic = json!({});
    for key in [
        "status",
        "reason",
        "code",
        "upstream_transient",
        "safe_to_retry",
    ] {
        if let Some(value) = enrichment.get(key) {
            diagnostic[key] = value.clone();
        }
    }
    diagnostic
}

fn status(state: &str, reason: Option<&str>) -> Value {
    let mut value = json!({"status": state});
    if let Some(reason) = reason {
        value["reason"] = json!(reason);
    }
    value
}

fn normalize_seller(raw: &Value, listing_id: &str) -> Value {
    let body = raw.get("data").unwrap_or(raw);
    let item = &body["item"];
    if super::identifier(item.get("id")).as_deref() != Some(listing_id) {
        return status("unknown", Some("missing_or_mismatched_item"));
    }
    let Some(user) = item.get("user").and_then(Value::as_object) else {
        return status("unknown", Some("missing_seller"));
    };
    let exposed = user.get("expose_location").and_then(Value::as_bool);
    let mut seller = json!({"expose_location": exposed});
    for field in ["feedback_count", "item_count"] {
        if let Some(value) = user.get(field).and_then(Value::as_u64) {
            seller[field] = json!(value);
        }
    }
    if let Some(value) = user.get("feedback_reputation").and_then(Value::as_f64)
        && (0.0..=1.0).contains(&value)
    {
        seller["feedback_reputation"] = json!(value);
    }
    if let Some(value) = user.get("is_on_holiday").and_then(Value::as_bool) {
        seller["is_on_holiday"] = json!(value);
    }
    let reason = if exposed != Some(true) {
        Some(if exposed == Some(false) {
            "location_hidden"
        } else {
            "location_visibility_unknown"
        })
    } else {
        if let Some(city) = nonempty_string(user.get("city")) {
            seller["city"] = json!(city);
        }
        match nonempty_string(user.get("country_title_local")) {
            Some(label) => {
                seller["country_label"] = json!(label);
                seller["country_source"] = json!("item.user.country_title_local");
                if label == "Suomi" {
                    seller["country_code"] = json!("FI");
                    None
                } else {
                    Some("country_unrecognized")
                }
            }
            None => Some("country_missing"),
        }
    };
    seller["status"] = json!(if reason.is_some() {
        "unknown"
    } else {
        "available"
    });
    if let Some(reason) = reason {
        seller["reason"] = json!(reason);
    }
    seller
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn money(raw: &Value) -> Option<Value> {
    let amount = match raw.get("amount")? {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    let amount = amount.parse::<DecimalAmount>().ok()?;
    let currency = raw.get("currency_code")?.as_str()?;
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(json!({"amount": amount.as_str(), "currency": currency}))
}

fn normalize_shipping(raw: &Value) -> Value {
    let body = raw.get("data").unwrap_or(raw);
    let services = &body["services"];
    let shipping = &services["shipping"];
    if services["pickup_only"] == true || shipping["pickup_only"] == true {
        return json!({"status": "pickup_only", "pickup_only": true});
    }
    let Some(price) = money(&shipping["final_price"]) else {
        return status("unknown", Some("missing_or_invalid_quote"));
    };
    let mut quote = json!({
        "status": "available",
        "kind": "starting_quote",
        "source": "item_pricing_services",
        "checkout_guaranteed": false,
        "account_item_time_dependent": true,
        "final_price": price,
    });
    for field in ["original_price", "discount"] {
        if let Some(value) = money(&shipping[field])
            && value["currency"] == quote["final_price"]["currency"]
        {
            quote[field] = value;
        }
    }
    if let Some(value) = shipping["discount_percentage"].as_f64()
        && (0.0..=100.0).contains(&value)
    {
        quote["discount_percentage"] = json!(value);
    }
    let info = &shipping["pricing_rule"]["additional_info"];
    if let Some(value) = info["multiple_shipping_options_available"].as_bool() {
        quote["multiple_shipping_options_available"] = json!(value);
    }
    if let Some(value @ ("all" | "point" | "home")) = info["pickup_type"].as_str() {
        quote["pickup_type"] = json!(value);
    }
    let discount = &shipping["discount_rule"];
    for (source, target) in [
        ("start_date", "discount_start_date"),
        ("end_date", "discount_end_date"),
    ] {
        if let Some(value) = discount[source].as_str().and_then(discount_date) {
            quote[target] = json!(value);
        }
    }
    if let Some(value @ ("all" | "point" | "home")) = discount["pickup_type"].as_str() {
        quote["discount_pickup_type"] = json!(value);
    }
    quote
}

fn discount_date(value: &str) -> Option<&str> {
    // Only valid UTC timestamps with second precision are disclosed.
    if value.len() != 20
        || value.bytes().enumerate().any(|(index, byte)| match index {
            4 | 7 => byte != b'-',
            10 => byte != b'T',
            13 | 16 => byte != b':',
            19 => byte != b'Z',
            _ => !byte.is_ascii_digit(),
        })
    {
        return None;
    }
    time::Date::from_calendar_date(
        value[0..4].parse().ok()?,
        time::Month::try_from(value[5..7].parse::<u8>().ok()?).ok()?,
        value[8..10].parse().ok()?,
    )
    .ok()?;
    time::Time::from_hms(
        value[11..13].parse().ok()?,
        value[14..16].parse().ok()?,
        value[17..19].parse().ok()?,
    )
    .ok()?;
    Some(value)
}

fn country_filter(seller: &Value, requested: bool) -> Value {
    if !requested {
        status("not_requested", None)
    } else if seller["country_code"] == "FI" && seller["status"] == "available" {
        status("matched", None)
    } else {
        status("excluded", Some("seller_country_unknown"))
    }
}

fn shipping_filter(shipping: &Value, maximum: Option<&DecimalAmount>) -> Value {
    let Some(maximum) = maximum else {
        return status("not_requested", None);
    };
    if shipping["status"] == "pickup_only" {
        return status("excluded", Some("pickup_only"));
    }
    if shipping["status"] != "available" {
        return status("excluded", Some("shipping_unknown"));
    }
    if shipping["final_price"]["currency"] != "EUR" {
        return status("excluded", Some("currency_not_eur"));
    }
    match shipping["final_price"]["amount"]
        .as_str()
        .and_then(|amount| amount.parse::<DecimalAmount>().ok())
    {
        Some(amount) if amount <= *maximum => status("matched", None),
        Some(_) => status("excluded", Some("shipping_above_maximum")),
        None => status("excluded", Some("shipping_unknown")),
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Mutex};

    use super::*;
    use crate::marketplace::{PortalId, vinted::search::CatalogueRequest};

    fn seller_fixture(id: &str) -> Value {
        json!({"item": {"id": id, "user": {
            "expose_location": true, "country_title_local": "Suomi", "city": "Example city",
            "feedback_count": 12, "feedback_reputation": 0.9, "item_count": 8,
            "is_on_holiday": false
        }}})
    }

    fn pricing_fixture() -> Value {
        json!({"services": {
            "pickup_only": false,
            "shipping": {
                "pickup_only": false,
                "final_price": {"amount": "0", "currency_code": "EUR"},
                "original_price": {"amount": "4.19", "currency_code": "EUR"},
                "discount": {"amount": "4.19", "currency_code": "EUR"},
                "discount_percentage": 100,
                "discount_rule": {
                    "landing_page_uri": "/not-for-output",
                    "start_date": "2026-09-01T21:00:00Z", "end_date": "2026-10-31T21:00:00Z",
                    "pickup_type": "point"
                },
                "pricing_rule": {"additional_info": {
                    "multiple_shipping_options_available": true, "pickup_type": "all"
                }}
            },
            "buyer_protection": {"final_price": {"amount": "2.70", "currency_code": "EUR"}}
        }, "total_amount": {"amount": "42.70", "currency_code": "EUR"}})
    }

    struct FakeApi {
        calls: Mutex<Vec<(String, bool)>>,
        fail: bool,
        fail_on: Option<(String, bool)>,
        pending: bool,
        responses: Vec<(String, bool, Value)>,
    }

    impl FakeApi {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail: false,
                fail_on: None,
                pending: false,
                responses: Vec::new(),
            }
        }
    }

    impl VintedSearchApi for FakeApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _request: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async { panic!("enrichment must not issue catalogue requests") })
        }

        fn enrichment<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            listing_id: &'a str,
            shipping: bool,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.calls
                .lock()
                .unwrap()
                .push((listing_id.to_owned(), shipping));
            Box::pin(async move {
                if self.pending {
                    std::future::pending::<()>().await;
                }
                if self.fail
                    || self
                        .fail_on
                        .as_ref()
                        .is_some_and(|(id, pricing)| id == listing_id && *pricing == shipping)
                {
                    return Err(AppError::upstream("fixture.failed", "secret response body"));
                }
                if let Some((_, _, response)) = self
                    .responses
                    .iter()
                    .find(|(id, pricing, _)| id == listing_id && *pricing == shipping)
                {
                    return Ok(response.clone());
                }
                Ok(if shipping {
                    pricing_fixture()
                } else {
                    seller_fixture(listing_id)
                })
            })
        }
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: "fixture".into(),
            login: None,
            access_token: "fixture".into(),
            refresh_token: "fixture".into(),
            access_expires_at_unix: u64::MAX,
            device_uuid: "fixture".into(),
            anonymous_id: "fixture".into(),
            user_device_token: None,
        }
    }

    fn collection(count: usize) -> SearchCollection {
        serde_json::from_value(json!({
            "query": "coat",
            "results": (0..count).map(|id| json!({
                "listing_id": id.to_string(), "title": "Example coat", "url": "https://example.invalid/item",
                "vinted": {"buyer_protection_fee": {"amount": "2.70", "currency": "EUR"},
                    "price_including_buyer_protection": {"amount": "42.70", "currency": "EUR"}}
            })).collect::<Vec<_>>(),
            "pagination": {"page": 2, "limit": count, "returned": count, "total": 100,
                "has_next": true, "next_page": 3}
        })).unwrap()
    }

    #[test]
    fn country_inputs_are_validated_without_general_country_inference() {
        for country in ["FI", "fi", "Finland", "finLAND", "Suomi", "SUOMI"] {
            assert!(
                validate(&Options {
                    seller_country: Some(country.into()),
                    ..Options::default()
                })
                .is_ok()
            );
        }
        for country in ["SE", "Sweden", "", "Finnish", " FI"] {
            assert!(
                validate(&Options {
                    seller_country: Some(country.into()),
                    ..Options::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn seller_visibility_and_country_evidence_are_required() {
        let raw = seller_fixture("1");
        let result = normalize_seller(&raw, "1");
        assert_eq!(result["country_code"], "FI");
        assert_eq!(result["city"], "Example city");
        assert_eq!(result, normalize_seller(&json!({"data": raw}), "1"));
        for visibility in [json!(false), Value::Null, json!("true")] {
            let mut raw = seller_fixture("1");
            raw["item"]["user"]["expose_location"] = visibility;
            let result = normalize_seller(&raw, "1");
            assert_eq!(result["status"], "unknown");
            for field in ["country_code", "country_label", "city"] {
                assert!(result.get(field).is_none());
            }
            assert_eq!(country_filter(&result, true)["status"], "excluded");
        }
        for label in ["Finland", "FI", "Sverige", "suomi"] {
            let mut raw = seller_fixture("1");
            raw["item"]["user"]["country_title_local"] = json!(label);
            raw["item"]["user"]["country_id"] = json!(1);
            let result = normalize_seller(&raw, "1");
            assert_eq!(result["country_label"], label);
            assert_eq!(result["status"], "unknown");
            assert!(result.get("country_code").is_none());
        }
        assert_eq!(
            normalize_seller(&seller_fixture("2"), "1")["status"],
            "unknown"
        );
        assert_eq!(normalize_seller(&json!({}), "1")["status"], "unknown");
    }

    #[test]
    fn postage_is_a_minimized_starting_quote_not_a_delivered_total() {
        let raw = pricing_fixture();
        let quote = normalize_shipping(&raw);
        assert_eq!(quote["kind"], "starting_quote");
        assert_eq!(quote["checkout_guaranteed"], false);
        assert_eq!(quote["final_price"]["amount"], "0");
        assert_eq!(quote["original_price"]["amount"], "4.19");
        assert_eq!(quote["discount"]["amount"], "4.19");
        assert_eq!(quote["multiple_shipping_options_available"], true);
        assert_eq!(quote["discount_start_date"], "2026-09-01T21:00:00Z");
        assert_eq!(quote["discount_end_date"], "2026-10-31T21:00:00Z");
        assert_eq!(quote["discount_pickup_type"], "point");
        assert_eq!(quote, normalize_shipping(&json!({"data": raw})));
        for field in [
            "total_amount",
            "discount_rule",
            "buyer_protection",
            "seller_discount",
        ] {
            assert!(quote.get(field).is_none());
        }
        assert_eq!(
            shipping_filter(&quote, Some(&"0.00".parse().unwrap()))["status"],
            "matched"
        );
    }

    #[test]
    fn malformed_discount_context_is_not_disclosed() {
        for value in [
            "irrelevant",
            "2026-02-30T00:00:00Z",
            "2026-09-01T25:00:00Z",
            "2026-09-01T00:00:00+00:00",
            "2026-09-01T00:00:00🦀",
        ] {
            let mut raw = pricing_fixture();
            raw["services"]["shipping"]["discount_rule"]["start_date"] = json!(value);
            raw["services"]["shipping"]["discount_rule"]["pickup_type"] = json!("unknown");
            let quote = normalize_shipping(&raw);
            assert!(quote.get("discount_start_date").is_none());
            assert!(quote.get("discount_pickup_type").is_none());
            assert_eq!(quote["status"], "available");
        }
    }

    #[test]
    fn invalid_and_pickup_only_quotes_never_match() {
        for amount in [
            json!("-1"),
            json!(-1),
            json!("1e2"),
            json!("1.234"),
            json!(null),
            json!(true),
        ] {
            let mut raw = pricing_fixture();
            raw["services"]["shipping"]["final_price"]["amount"] = amount;
            let quote = normalize_shipping(&raw);
            assert_eq!(quote["status"], "unknown");
            assert_eq!(
                shipping_filter(&quote, Some(&"100".parse().unwrap()))["status"],
                "excluded"
            );
        }
        for currency in [Value::Null, json!(""), json!("eur"), json!("EURO")] {
            let mut raw = pricing_fixture();
            raw["services"]["shipping"]["final_price"]["currency_code"] = currency;
            assert_eq!(normalize_shipping(&raw)["status"], "unknown");
        }
        for top_level in [true, false] {
            let mut raw = pricing_fixture();
            if top_level {
                raw["services"]["pickup_only"] = json!(true);
            } else {
                raw["services"]["shipping"]["pickup_only"] = json!(true);
            }
            let quote = normalize_shipping(&raw);
            assert_eq!(quote["status"], "pickup_only");
            assert_eq!(
                shipping_filter(&quote, Some(&"100".parse().unwrap()))["reason"],
                "pickup_only"
            );
        }
        assert_eq!(normalize_shipping(&json!({}))["status"], "unknown");
    }

    #[test]
    fn shipping_filter_compares_decimals_and_requires_eur() {
        let mut raw = pricing_fixture();
        raw["services"]["shipping"]["final_price"]["amount"] = json!("10.01");
        assert_eq!(
            shipping_filter(&normalize_shipping(&raw), Some(&"9.99".parse().unwrap()))["reason"],
            "shipping_above_maximum"
        );
        assert_eq!(
            shipping_filter(&normalize_shipping(&raw), Some(&"10.01".parse().unwrap()))["status"],
            "matched"
        );
        raw["services"]["shipping"]["final_price"]["currency_code"] = json!("USD");
        assert_eq!(
            shipping_filter(&normalize_shipping(&raw), Some(&"100".parse().unwrap()))["reason"],
            "currency_not_eur"
        );
    }

    #[tokio::test]
    async fn default_options_do_not_request_or_change_anything() {
        let api = FakeApi::new();
        let mut result = collection(2);
        let original = result.clone();
        enrich(&api, &credentials(), &mut result, &Options::default()).await;
        assert_eq!(result, original);
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn filters_enable_dependencies_and_preserve_page_metadata_and_money() {
        let api = FakeApi::new();
        let mut result = collection(21);
        let original_page = result.pagination.clone();
        let options = Options {
            seller_country: Some("FI".into()),
            shipping_to: Some("0".parse().unwrap()),
            ..Options::default()
        };
        enrich(&api, &credentials(), &mut result, &options).await;
        assert_eq!(result.results.len(), 20);
        let mut expected_page = original_page;
        expected_page.returned = 20;
        assert_eq!(result.pagination, expected_page);
        let calls = api.calls.lock().unwrap();
        assert_eq!(calls.len(), 40);
        for (index, pair) in calls.chunks_exact(2).enumerate() {
            assert_eq!(
                pair,
                [(index.to_string(), false), (index.to_string(), true)]
            );
        }
        assert_eq!(
            result.results[0].vinted.as_ref().unwrap()["buyer_protection_fee"]["amount"],
            "2.70"
        );
        assert_eq!(
            result.results[0].vinted.as_ref().unwrap()["price_including_buyer_protection"]["amount"],
            "42.70"
        );
        let summary = result.enrichment.unwrap();
        assert_eq!(summary["scope"], "upstream_page");
        assert_eq!(summary["excluded"], 1);
        assert_eq!(summary["unprocessed"], 1);
        assert_eq!(summary["items"][20]["seller"]["reason"], "item_limit");
        assert_eq!(summary["items"].as_array().unwrap().len(), 21);
    }

    #[tokio::test]
    async fn failures_are_per_item_and_do_not_expose_error_bodies() {
        let api = FakeApi {
            fail: true,
            ..FakeApi::new()
        };
        for filter in [false, true] {
            let mut result = collection(3);
            let options = Options {
                include_shipping: true,
                shipping_to: filter.then(|| "5".parse().unwrap()),
                ..Options::default()
            };
            enrich(&api, &credentials(), &mut result, &options).await;
            assert_eq!(result.results.len(), if filter { 0 } else { 3 });
            let summary = result.enrichment.unwrap();
            assert_eq!(summary["errors"], 3);
            assert_eq!(summary["excluded"], if filter { 3 } else { 0 });
            assert_eq!(summary["items"][0]["seller"]["status"], "not_requested");
            assert_eq!(summary["items"][0]["shipping"]["code"], "fixture.failed");
            assert!(!summary.to_string().contains("secret response body"));
        }
    }

    #[tokio::test]
    async fn one_failed_request_preserves_successful_enrichment_and_continues() {
        let api = FakeApi {
            fail_on: Some(("1".into(), true)),
            ..FakeApi::new()
        };
        for filtered in [false, true] {
            let mut result = collection(3);
            enrich(
                &api,
                &credentials(),
                &mut result,
                &Options {
                    include_seller: true,
                    include_shipping: true,
                    shipping_to: filtered.then(|| "5".parse().unwrap()),
                    ..Options::default()
                },
            )
            .await;
            assert_eq!(result.results.len(), if filtered { 2 } else { 3 });
            assert_eq!(result.results.last().unwrap().listing_id, "2");
            assert_eq!(
                result.results.last().unwrap().vinted.as_ref().unwrap()["shipping"]["status"],
                "available"
            );
            if !filtered {
                assert_eq!(
                    result.results[1].vinted.as_ref().unwrap()["seller"]["country_code"],
                    "FI"
                );
                assert_eq!(
                    result.results[1].vinted.as_ref().unwrap()["shipping"]["status"],
                    "error"
                );
            }
            let summary = result.enrichment.unwrap();
            assert_eq!(summary["errors"], 1);
            assert_eq!(summary["excluded"], usize::from(filtered));
            assert_eq!(summary["items"][1]["shipping"]["code"], "fixture.failed");
            assert_eq!(
                summary["items"][1]["seller"],
                json!({"status": "available"})
            );
            assert_eq!(
                summary["items"][2]["shipping"],
                json!({"status": "available"})
            );
        }
        assert_eq!(api.calls.lock().unwrap().len(), 12);
    }

    #[tokio::test]
    async fn mixed_filters_report_unknown_and_excluded_items_without_losing_order() {
        let mut hidden = seller_fixture("1");
        hidden["item"]["user"]["expose_location"] = json!(false);
        let mut expensive = pricing_fixture();
        expensive["services"]["shipping"]["final_price"]["amount"] = json!("5.01");
        let mut foreign = pricing_fixture();
        foreign["services"]["shipping"]["final_price"]["currency_code"] = json!("USD");
        let mut pickup = pricing_fixture();
        pickup["services"]["pickup_only"] = json!(true);
        let api = FakeApi {
            responses: vec![
                ("1".into(), false, hidden),
                ("2".into(), true, json!({})),
                ("3".into(), true, expensive),
                ("4".into(), true, foreign),
                ("5".into(), true, pickup),
            ],
            ..FakeApi::new()
        };
        let mut result = collection(7);
        enrich(
            &api,
            &credentials(),
            &mut result,
            &Options {
                seller_country: Some("fi".into()),
                shipping_to: Some("5".parse().unwrap()),
                ..Options::default()
            },
        )
        .await;
        assert_eq!(
            result
                .results
                .iter()
                .map(|item| item.listing_id.as_str())
                .collect::<Vec<_>>(),
            ["0", "6"]
        );
        assert_eq!(result.pagination.total, 100);
        let summary = result.enrichment.unwrap();
        assert_eq!(summary["unknown"], 2);
        assert_eq!(summary["excluded"], 5);
        assert_eq!(summary["errors"], 0);
        assert_eq!(summary["coverage"]["complete"], false);
        assert_eq!(summary["coverage"]["fully_processed_items"], 7);
        assert_eq!(summary["filters"]["seller_country"], "FI");
        assert_eq!(summary["filters"]["shipping_to"], "5");
        assert_eq!(summary["items"][1]["seller"]["reason"], "location_hidden");
        assert_eq!(
            summary["items"][2]["shipping"]["reason"],
            "missing_or_invalid_quote"
        );
        assert_eq!(
            summary["items"][3]["filters"]["shipping_to"]["reason"],
            "shipping_above_maximum"
        );
        assert_eq!(
            summary["items"][4]["filters"]["shipping_to"]["reason"],
            "currency_not_eur"
        );
        assert_eq!(summary["items"][5]["shipping"]["status"], "pickup_only");
    }

    #[tokio::test]
    async fn item_limit_without_filters_retains_unprocessed_listings() {
        let api = FakeApi::new();
        let mut result = collection(22);
        enrich(
            &api,
            &credentials(),
            &mut result,
            &Options {
                include_seller: true,
                ..Options::default()
            },
        )
        .await;
        assert_eq!(result.results.len(), 22);
        assert_eq!(api.calls.lock().unwrap().len(), 20);
        assert_eq!(result.enrichment.as_ref().unwrap()["unprocessed"], 2);
        assert_eq!(
            result.results[21].vinted.as_ref().unwrap()["seller"]["reason"],
            "item_limit"
        );
    }

    #[tokio::test]
    async fn request_timeouts_continue_and_total_deadline_stops_requests() {
        let api = FakeApi {
            pending: true,
            ..FakeApi::new()
        };
        let options = Options {
            include_seller: true,
            include_shipping: true,
            ..Options::default()
        };
        let mut result = collection(2);
        enrich_with_limits(
            &api,
            &credentials(),
            &mut result,
            &options,
            Limits {
                request: Duration::ZERO,
                ..LIMITS
            },
        )
        .await;
        assert_eq!(api.calls.lock().unwrap().len(), 4);
        assert_eq!(result.enrichment.as_ref().unwrap()["errors"], 2);
        assert_eq!(
            result.enrichment.as_ref().unwrap()["items"][0]["seller"]["reason"],
            "request_timeout"
        );
        let deadline_result = fetch(
            &api,
            &credentials(),
            "0",
            true,
            Instant::now(),
            LIMITS.request,
        )
        .await;
        assert_eq!(deadline_result["reason"], "total_deadline");
        let api = FakeApi::new();
        let mut result = collection(2);
        enrich_with_limits(
            &api,
            &credentials(),
            &mut result,
            &options,
            Limits {
                total: Duration::ZERO,
                ..LIMITS
            },
        )
        .await;
        assert!(api.calls.lock().unwrap().is_empty());
        assert_eq!(result.enrichment.as_ref().unwrap()["unprocessed"], 2);
        assert_eq!(
            result.enrichment.as_ref().unwrap()["items"][0]["seller"]["reason"],
            "total_deadline"
        );
    }
}
