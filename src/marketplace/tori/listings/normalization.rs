use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    ListingsApiError, UpstreamAction, UpstreamFacet, UpstreamListing, UpstreamListingSummary,
    UpstreamState, UpstreamStatistics, UpstreamSummaryData, unexpected,
};
use crate::{
    domain::{
        commerce::{Price, PriceKind, TradeType, normalize_commerce_fields},
        listing::{
            ListingAction, ListingActionName, ListingDetail, ListingFacet, ListingSnapshot,
            ListingState, ListingStatistics, ListingSummary,
        },
    },
    error::AppError,
};

pub(super) fn normalize_summary(
    raw: UpstreamListingSummary,
    mut trade_type: TradeType,
    mut price: Price,
) -> Result<ListingSummary, AppError> {
    let listing_id = value_id(&raw.id)?;
    enrich_commerce_from_tori_subtitle(&raw.data.subtitle, &mut trade_type, &mut price);
    if price.display.is_none() {
        price.display = nonempty(raw.data.subtitle.clone());
    }
    Ok(ListingSummary {
        public_url: public_listing_url(&listing_id, &raw.data.public_url),
        listing_id,
        title: raw.data.title,
        trade_type,
        price,
        state: normalize_state(&raw.state),
        location: nonempty(raw.data.location),
        image_url: nonempty(raw.data.image),
        created_at: raw.created,
        updated_at: raw.updated,
        expires_at: raw.expires,
        days_until_expires: raw.days_until_expires,
        statistics: normalize_statistics(&raw.external_data),
        actions: raw.actions.into_iter().map(normalize_action).collect(),
    })
}

pub(super) fn normalize_listing_detail_for_id(
    mut raw: UpstreamListing,
    expected_id: &str,
) -> Result<ListingDetail, AppError> {
    let listing_id = value_id(&raw.id)?;
    if listing_id != expected_id {
        return Err(unexpected("listing detail returned a different ID"));
    }
    merge_summary_fields(&mut raw.fields, &raw.data, &listing_id);
    let (mut trade_type, mut price) = commerce_from_fields(&raw.fields);
    enrich_commerce_from_tori_subtitle(&raw.data.subtitle, &mut trade_type, &mut price);
    let fields = display_fields(&raw.fields);
    Ok(ListingDetail {
        listing_id,
        state: normalize_state(&raw.state),
        trade_type,
        price,
        fields,
        statistics: normalize_statistics(&raw.external_data),
        actions: raw.actions.into_iter().map(normalize_action).collect(),
    })
}

fn enrich_commerce_from_tori_subtitle(
    subtitle: &str,
    trade_type: &mut TradeType,
    price: &mut Price,
) {
    let mut parts = subtitle.split_whitespace();
    let Some(marketplace) = parts.next() else {
        return;
    };
    let Some(trade_label) = parts.next() else {
        return;
    };
    if !marketplace.eq_ignore_ascii_case("Tori") {
        return;
    }
    let inferred_trade =
        crate::domain::commerce::normalize_trade_type(Some(&Value::String(trade_label.to_owned())));
    if inferred_trade == TradeType::Unknown {
        return;
    }
    let amount_text = parts.collect::<Vec<_>>().join(" ");
    let amount = euro_amount(&amount_text);
    let mut fields = serde_json::Map::new();
    fields.insert(
        "trade_type".to_owned(),
        Value::String(
            inferred_trade
                .normalized_value()
                .expect("recognized trade type has a normalized value")
                .to_owned(),
        ),
    );
    if let Some(amount) = amount {
        fields.insert("price".to_owned(), Value::Number(amount));
        fields.insert("currency".to_owned(), Value::String("EUR".to_owned()));
    }
    fields.insert(
        "price_display".to_owned(),
        Value::String(subtitle.to_owned()),
    );
    let (inferred_trade, inferred_price) = normalize_commerce_fields(&fields);
    if *trade_type == TradeType::Unknown {
        *trade_type = inferred_trade;
    }
    if price.kind == PriceKind::Unavailable {
        *price = inferred_price;
    }
}

fn euro_amount(value: &str) -> Option<serde_json::Number> {
    let value = value.trim().strip_suffix('€')?.trim();
    if value.is_empty() {
        return None;
    }
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, ' ' | '\u{a0}' | '\u{202f}'))
        .map(|character| if character == ',' { '.' } else { character })
        .collect::<String>();
    if !normalized
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.')
        || normalized.matches('.').count() > 1
    {
        return None;
    }
    normalized.parse().ok()
}

pub(super) fn summary_detail(summary: ListingSummary) -> ListingDetail {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_owned(), Value::String(summary.title));
    if let Some(location) = summary.location {
        fields.insert("location".to_owned(), Value::String(location));
    }
    if let Some(image) = summary.image_url {
        fields.insert("image".to_owned(), Value::String(image));
    }
    fields.insert("public_url".to_owned(), Value::String(summary.public_url));
    ListingDetail {
        listing_id: summary.listing_id,
        state: summary.state,
        trade_type: summary.trade_type,
        price: summary.price,
        fields,
        statistics: summary.statistics,
        actions: summary.actions,
    }
}

fn merge_summary_fields(
    fields: &mut BTreeMap<String, Value>,
    data: &UpstreamSummaryData,
    listing_id: &str,
) {
    for (key, value) in [
        ("title", data.title.as_str()),
        ("price_display", data.subtitle.as_str()),
        ("location", data.location.as_str()),
        ("image", data.image.as_str()),
    ] {
        if !value.is_empty() {
            fields
                .entry(key.to_owned())
                .or_insert_with(|| Value::String(value.to_owned()));
        }
    }
    fields
        .entry("public_url".to_owned())
        .or_insert_with(|| Value::String(public_listing_url(listing_id, &data.public_url)));
}

fn public_listing_url(listing_id: &str, upstream: &str) -> String {
    if upstream.starts_with("https://www.tori.fi/") {
        upstream.to_owned()
    } else {
        format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}")
    }
}

pub(super) fn detail_observation_status(error: &ListingsApiError) -> &'static str {
    match error {
        ListingsApiError::NotFound => "not_found",
        ListingsApiError::UnexpectedResponse(_) => "unrecognized_model",
        ListingsApiError::Transport | ListingsApiError::Upstream(_) => "unavailable",
        _ => "rejected",
    }
}

pub(super) fn summary_id(value: &Value) -> Result<String, ListingsApiError> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(ListingsApiError::UnexpectedResponse(
            "listing summary has an invalid ID".to_owned(),
        )),
    }
}

pub(super) fn normalize_listing_for_id(
    raw: UpstreamListing,
    expected_id: &str,
) -> Result<ListingSnapshot, AppError> {
    let snapshot = normalize_listing(raw)?;
    if snapshot.detail.listing_id != expected_id {
        return Err(unexpected("listing detail returned a different ID"));
    }
    Ok(snapshot)
}

fn normalize_listing(raw: UpstreamListing) -> Result<ListingSnapshot, AppError> {
    if raw.etag.trim().is_empty() {
        return Err(unexpected("listing detail omitted its ETag"));
    }
    let source_fields = raw.fields;
    let (trade_type, price) = commerce_from_fields(&source_fields);
    Ok(ListingSnapshot {
        detail: ListingDetail {
            listing_id: value_id(&raw.id)?,
            state: normalize_state(&raw.state),
            trade_type,
            price,
            fields: display_fields(&source_fields),
            statistics: normalize_statistics(&raw.external_data),
            actions: raw.actions.into_iter().map(normalize_action).collect(),
        },
        etag: raw.etag,
        source_fields,
    })
}

pub(super) fn commerce_from_fields(fields: &BTreeMap<String, Value>) -> (TradeType, Price) {
    let object = fields
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    normalize_commerce_fields(&object)
}

fn display_fields(fields: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    fields
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "price"
                    | "price_amount"
                    | "priceAmount"
                    | "currency"
                    | "currencyCode"
                    | "currency_code"
                    | "trade_type"
                    | "tradeType"
                    | "adViewTypeLabel"
                    | "price_display"
                    | "priceText"
                    | "price_text"
                    | "subtitle"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub(super) fn normalize_facet(raw: UpstreamFacet) -> ListingFacet {
    ListingFacet {
        state: normalize_state_name(&raw.name),
        label: raw.label,
        total: raw.total,
    }
}

fn normalize_state(raw: &UpstreamState) -> ListingState {
    let value = [&raw.state_type, &raw.display, &raw.label]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map_or("", String::as_str);
    normalize_state_name(value)
}

fn normalize_state_name(value: &str) -> ListingState {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" => ListingState::All,
        "active" | "published" => ListingState::Active,
        "pending" | "review" => ListingState::Pending,
        "expired" => ListingState::Expired,
        "disposed" | "sold" => ListingState::Disposed,
        "draft" => ListingState::Draft,
        _ => ListingState::Unknown,
    }
}

fn normalize_action(raw: UpstreamAction) -> ListingAction {
    let name = match raw.name.trim().to_ascii_lowercase().as_str() {
        "edit" => ListingActionName::Edit,
        "dispose" => ListingActionName::Dispose,
        "delete" => ListingActionName::Delete,
        "republish" | "recreate" => ListingActionName::Republish,
        "undispose" | "activate" => ListingActionName::Undispose,
        "view" | "show" => ListingActionName::View,
        _ => ListingActionName::Unknown,
    };
    ListingAction {
        name,
        label: raw.label,
        method: raw.method.to_ascii_uppercase(),
    }
}

fn normalize_statistics(raw: &UpstreamStatistics) -> ListingStatistics {
    ListingStatistics {
        views: parse_statistic(&raw.clicks.value),
        favorites: parse_statistic(&raw.favorites.value),
    }
}

fn parse_statistic(value: &str) -> Option<u64> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

#[cfg(test)]
pub(super) fn collect_image_urls(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(url) if !url.is_empty() => output.push(url.clone()),
        Value::Array(values) => {
            for value in values {
                collect_image_urls(value, output);
            }
        }
        Value::Object(object) => {
            for key in ["url", "uri", "src", "image_url"] {
                if let Some(Value::String(url)) = object.get(key)
                    && !url.is_empty()
                {
                    output.push(url.clone());
                    break;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn value_id(value: &Value) -> Result<String, AppError> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(unexpected("listing has an invalid ID")),
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}
