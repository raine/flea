use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceKind {
    Fixed,
    Free,
    Negotiable,
    NotApplicable,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub kind: PriceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl Price {
    pub fn unavailable(display: Option<String>) -> Self {
        Self {
            kind: PriceKind::Unavailable,
            amount: None,
            currency: None,
            display,
        }
    }
}

pub fn normalize_trade_type(value: Option<&Value>) -> TradeType {
    let Some(value) = value else {
        return TradeType::Unknown;
    };
    let value = match value {
        Value::String(value) => value.trim().to_ascii_lowercase(),
        Value::Number(value) => value.to_string(),
        _ => return TradeType::Unknown,
    };
    match value.as_str() {
        "1" | "sell" | "selling" | "myydään" => TradeType::Sell,
        "2" | "give_away" | "give-away" | "free" | "annetaan" => TradeType::GiveAway,
        "3" | "wanted" | "buy" | "ostetaan" => TradeType::Wanted,
        _ => TradeType::Unknown,
    }
}

impl TradeType {
    pub fn normalized_value(self) -> Option<&'static str> {
        match self {
            Self::Sell => Some("sell"),
            Self::GiveAway => Some("give_away"),
            Self::Wanted => Some("wanted"),
            Self::Unknown => None,
        }
    }

    pub fn machine_value(self) -> Option<&'static str> {
        match self {
            Self::Sell => Some("1"),
            Self::GiveAway => Some("2"),
            Self::Wanted => Some("3"),
            Self::Unknown => None,
        }
    }
}

pub fn normalized_select_to_machine(field: &str, value: &Value) -> Option<Value> {
    match field {
        "trade_type" | "trade-type" | "tradeType" => normalize_trade_type(Some(value))
            .machine_value()
            .map(|value| Value::String(value.to_owned())),
        _ => Some(value.clone()),
    }
}

pub fn machine_select_to_normalized(field: &str, value: &Value) -> Value {
    match field {
        "trade_type" | "trade-type" | "tradeType" => normalize_trade_type(Some(value))
            .normalized_value()
            .map_or_else(
                || Value::String("unknown".to_owned()),
                |value| Value::String(value.to_owned()),
            ),
        _ => value.clone(),
    }
}

pub fn select_values_equal(field: &str, left: &Value, right: &Value) -> bool {
    match (
        normalized_select_to_machine(field, left),
        normalized_select_to_machine(field, right),
    ) {
        (Some(left), Some(right)) => scalar_values_equal(&left, &right),
        _ => false,
    }
}

fn scalar_values_equal(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Value::String(left), Value::Number(right)) => left == &right.to_string(),
        (Value::Number(left), Value::String(right)) => &left.to_string() == right,
        _ => false,
    }
}

pub fn normalize_price(
    value: Option<&Value>,
    currency: Option<&Value>,
    display: Option<String>,
    trade_type: TradeType,
) -> Price {
    let negotiated = value.and_then(Value::as_object).is_some_and(|object| {
        bool_value(object, &["negotiable", "isNegotiable"])
            || string_value(object, &["type", "kind", "price_type"])
                .is_some_and(|value| value.eq_ignore_ascii_case("negotiable"))
    });
    let free = value.and_then(Value::as_object).is_some_and(|object| {
        bool_value(object, &["free", "isFree"])
            || string_value(object, &["type", "kind", "price_type"])
                .is_some_and(|value| value.eq_ignore_ascii_case("free"))
    });
    let amount = value.and_then(numeric_amount);
    let source_currency = value
        .and_then(Value::as_object)
        .and_then(|object| value_at(object, &["currency", "currencyCode", "currency_code"]))
        .or(currency);
    let currency = amount.as_ref().and_then(|_| match source_currency {
        Some(value) => value.as_str().and_then(normalize_currency),
        None => Some("EUR".to_owned()),
    });
    let kind = if free || trade_type == TradeType::GiveAway {
        PriceKind::Free
    } else if negotiated {
        PriceKind::Negotiable
    } else if amount.is_some() {
        PriceKind::Fixed
    } else {
        match trade_type {
            TradeType::Wanted => PriceKind::NotApplicable,
            TradeType::Sell | TradeType::Unknown => PriceKind::Unavailable,
            TradeType::GiveAway => unreachable!("give-away handled above"),
        }
    };
    let (amount, currency) = if kind == PriceKind::Free {
        (None, None)
    } else {
        (amount, currency)
    };
    Price {
        kind,
        amount,
        currency,
        display,
    }
}

pub fn normalize_commerce_fields(fields: &Map<String, Value>) -> (TradeType, Price) {
    let trade_source = value_at(fields, &["trade_type", "tradeType", "adViewTypeLabel"]);
    let trade_type = normalize_trade_type(trade_source);
    let price_value = value_at(fields, &["price", "price_amount", "priceAmount"]);
    let currency = value_at(fields, &["currency", "currencyCode", "currency_code"]);
    let display = value_at(
        fields,
        &["price_display", "priceText", "price_text", "subtitle"],
    )
    .and_then(Value::as_str)
    .filter(|value| !value.trim().is_empty())
    .map(str::to_owned)
    .or_else(|| {
        price_value
            .and_then(Value::as_object)
            .and_then(|object| value_at(object, &["display", "formatted", "text"]))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    (
        trade_type,
        normalize_price(price_value, currency, display, trade_type),
    )
}

pub fn normalize_values_output(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(normalize_values_output),
        Value::Object(object) => {
            if let Some(Value::Object(values)) = object.get_mut("values") {
                normalize_commerce_map(values);
            }
            for child in object.values_mut() {
                normalize_values_output(child);
            }
        }
        _ => {}
    }
}

pub fn normalize_commerce_map(values: &mut Map<String, Value>) {
    let trade_source = value_at(values, &["trade_type", "tradeType", "adViewTypeLabel"]);
    let (_, price) = normalize_commerce_fields(values);
    values.insert(
        "trade_type".to_owned(),
        trade_source.map_or_else(
            || Value::String("unknown".to_owned()),
            |value| machine_select_to_normalized("trade_type", value),
        ),
    );
    values.insert(
        "price".to_owned(),
        serde_json::to_value(price).expect("price serializes"),
    );
}

fn numeric_amount(value: &Value) -> Option<Value> {
    match value {
        Value::Number(_) => Some(value.clone()),
        Value::String(value) => value.parse::<Number>().ok().map(Value::Number),
        Value::Array(values) if values.len() == 1 => numeric_amount(&values[0]),
        Value::Object(object) => value_at(
            object,
            &[
                "amount",
                "value",
                "price_amount",
                "priceAmount",
                "price_max",
            ],
        )
        .and_then(numeric_amount),
        _ => None,
    }
}

fn normalize_currency(value: &str) -> Option<String> {
    let currency = value.trim();
    (currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then(|| currency.to_ascii_uppercase())
}

fn value_at<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| object.get(*name))
}

fn string_value<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    value_at(object, names).and_then(Value::as_str)
}

fn bool_value(object: &Map<String, Value>, names: &[&str]) -> bool {
    value_at(object, names).and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_normalized_select_values_to_machine_values() {
        for (input, machine, normalized) in [
            (json!("sell"), json!("1"), json!("sell")),
            (json!("give_away"), json!("2"), json!("give_away")),
            (json!("wanted"), json!("3"), json!("wanted")),
            (json!(1), json!("1"), json!("sell")),
            (json!(2), json!("2"), json!("give_away")),
            (json!(3), json!("3"), json!("wanted")),
        ] {
            assert_eq!(
                normalized_select_to_machine("trade_type", &input),
                Some(machine)
            );
            assert_eq!(
                machine_select_to_normalized("trade_type", &input),
                normalized
            );
        }
        assert_eq!(
            normalized_select_to_machine("trade_type", &json!("4")),
            None
        );
        assert_eq!(
            machine_select_to_normalized("trade_type", &json!("4")),
            json!("unknown")
        );
        assert!(select_values_equal("trade_type", &json!("sell"), &json!(1)));
    }

    #[test]
    fn normalizes_price_semantics_without_parsing_display_text() {
        for (fields, trade_type, kind, amount) in [
            (
                json!({"trade_type":"sell","price":5}),
                TradeType::Sell,
                PriceKind::Fixed,
                Some(json!(5)),
            ),
            (
                json!({"trade_type":"sell","price":0}),
                TradeType::Sell,
                PriceKind::Fixed,
                Some(json!(0)),
            ),
            (
                json!({"trade_type":"give_away","price":0}),
                TradeType::GiveAway,
                PriceKind::Free,
                None,
            ),
            (
                json!({"trade_type":"wanted"}),
                TradeType::Wanted,
                PriceKind::NotApplicable,
                None,
            ),
            (
                json!({"trade_type":"sell"}),
                TradeType::Sell,
                PriceKind::Unavailable,
                None,
            ),
            (
                json!({"trade_type":"sell","price":"5.250"}),
                TradeType::Sell,
                PriceKind::Fixed,
                Some(json!(5.250)),
            ),
            (
                json!({"trade_type":"sell","price":{"negotiable":true}}),
                TradeType::Sell,
                PriceKind::Negotiable,
                None,
            ),
            (
                json!({"trade_type":"sell","price":{"free":true}}),
                TradeType::Sell,
                PriceKind::Free,
                None,
            ),
        ] {
            let (actual_trade, price) = normalize_commerce_fields(fields.as_object().unwrap());
            assert_eq!(actual_trade, trade_type);
            assert_eq!(price.kind, kind);
            assert_eq!(price.amount, amount);
        }

        let fields = json!({
            "trade_type": "unexpected",
            "price": {"amount": "not localized machine data", "currency": "euros", "display": "Tori myydään 5 €"}
        });
        let (trade_type, price) = normalize_commerce_fields(fields.as_object().unwrap());
        assert_eq!(trade_type, TradeType::Unknown);
        assert_eq!(price.kind, PriceKind::Unavailable);
        assert_eq!(price.amount, None);
        assert_eq!(price.currency, None);
        assert_eq!(price.display.as_deref(), Some("Tori myydään 5 €"));
    }
}
