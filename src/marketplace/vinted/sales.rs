use std::{future::Future, pin::Pin};

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use super::{
    auth::{VintedAuthentication, VintedCredentialRecord},
    binding::VINTED_FI_BINDING,
    item::VintedItemSession,
};
use crate::{
    domain::{
        envelope::NextAction,
        search::SearchPrice,
        vinted_sale::{
            VintedSale, VintedSalesCollection, VintedSalesPagination, VintedSalesStatus,
        },
    },
    error::AppError,
    marketplace::PortalId,
    transport::{Transport, TransportError, TransportErrorKind, TransportResponse},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VintedSalesRequest {
    pub status: VintedSalesStatus,
    pub page: usize,
    pub limit: usize,
}

impl Default for VintedSalesRequest {
    fn default() -> Self {
        Self {
            status: VintedSalesStatus::Completed,
            page: 1,
            limit: 20,
        }
    }
}

impl VintedSalesRequest {
    fn validate(&self) -> Result<(), AppError> {
        if self.page == 0 || self.page > i32::MAX as usize {
            return Err(AppError::usage(
                "sales page must be between 1 and 2147483647",
            ));
        }
        if !(1..=96).contains(&self.limit) {
            return Err(AppError::usage("sales limit must be between 1 and 96"));
        }
        Ok(())
    }
}

pub trait VintedSalesApi: Send + Sync {
    fn orders<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a VintedSalesRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedSales<'a> {
    session: &'a dyn VintedItemSession,
    api: &'a dyn VintedSalesApi,
}

impl<'a> VintedSales<'a> {
    pub fn new(session: &'a dyn VintedItemSession, api: &'a dyn VintedSalesApi) -> Self {
        Self { session, api }
    }

    pub async fn list(
        &self,
        portal: PortalId,
        request: VintedSalesRequest,
    ) -> Result<VintedSalesCollection, AppError> {
        request.validate()?;
        let credentials = self.session.credentials(portal).await?;
        let raw = self.api.orders(&credentials, &request).await?;
        normalize(raw, &request)
    }
}

pub struct HttpVintedSalesApi {
    auth: VintedAuthentication,
}

impl HttpVintedSalesApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
        }
    }

    async fn get(
        &self,
        credentials: &VintedCredentialRecord,
        request: &VintedSalesRequest,
    ) -> Result<Value, AppError> {
        let url = endpoint(VINTED_FI_BINDING.portal_api_host, request)?;
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(transport_error)?;
        decode_response(response)
    }
}

impl Default for HttpVintedSalesApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedSalesApi for HttpVintedSalesApi {
    fn orders<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a VintedSalesRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.get(credentials, request))
    }
}

fn endpoint(base: &str, request: &VintedSalesRequest) -> Result<Url, AppError> {
    request.validate()?;
    let mut url =
        Url::parse(base).map_err(|_| AppError::unexpected("Vinted API binding is invalid"))?;
    url.set_path("/api/v2/my_orders");
    url.query_pairs_mut()
        .append_pair("type", "sold")
        .append_pair("status", request.status.as_str())
        .append_pair("page", &request.page.to_string())
        .append_pair("per_page", &request.limit.to_string());
    Ok(url)
}

#[derive(Deserialize)]
struct OrdersResponse {
    my_orders: Vec<Order>,
    pagination: VintedSalesPagination,
}

#[derive(Deserialize)]
struct Order {
    #[serde(default, deserialize_with = "optional_id")]
    transaction_id: Option<String>,
    #[serde(default, deserialize_with = "optional_id")]
    conversation_id: Option<String>,
    title: Option<String>,
    price: Option<Price>,
    status: Option<String>,
    transaction_user_status: Option<String>,
    photo: Option<Photo>,
    date: Option<String>,
}

#[derive(Deserialize)]
struct Photo {
    url: Option<String>,
}

#[derive(Deserialize)]
struct Price {
    amount: Value,
    currency_code: Option<String>,
}

fn optional_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Value::String(id)) => Ok((!id.is_empty()).then_some(id)),
        Some(Value::Number(id)) if id.as_u64().is_some() => Ok(Some(id.to_string())),
        _ => Err(serde::de::Error::custom("unsupported order identifier")),
    }
}

fn normalize(raw: Value, request: &VintedSalesRequest) -> Result<VintedSalesCollection, AppError> {
    if raw.get("code").is_some_and(|code| code.as_u64() != Some(0)) {
        return Err(invalid_response(
            "upstream reported an unsuccessful response",
        ));
    }
    let response: OrdersResponse = serde_json::from_value(raw)
        .map_err(|_| invalid_response("orders or pagination had an unsupported shape"))?;
    let pagination = response.pagination;
    let count = response.my_orders.len();
    if pagination.current_page != request.page
        || pagination.total_pages > i32::MAX as usize
        || count > request.limit
        || count > pagination.total_entries
        || (pagination.total_entries == 0 && (count != 0 || pagination.total_pages > 1))
        || (pagination.total_entries > 0 && pagination.total_pages == 0)
        || (count > 0 && request.page > pagination.total_pages)
        || (count == 0 && request.page < pagination.total_pages)
    {
        return Err(invalid_response(
            "pagination was inconsistent with the requested page",
        ));
    }
    let sales = response
        .my_orders
        .into_iter()
        .map(|order| {
            let price = order
                .price
                .map(|price| {
                    let amount = match price.amount {
                        Value::Number(number) => number,
                        Value::String(text) => text
                            .parse()
                            .map_err(|_| invalid_response("order price was not numeric"))?,
                        _ => return Err(invalid_response("order price was not numeric")),
                    };
                    Ok(SearchPrice {
                        amount: Value::Number(amount),
                        currency: price.currency_code,
                    })
                })
                .transpose()?;
            Ok(VintedSale {
                transaction_id: order.transaction_id,
                conversation_id: order.conversation_id,
                title: order.title,
                price,
                status_text: order.status,
                transaction_user_status: order.transaction_user_status,
                photo_url: order.photo.and_then(|photo| photo.url),
                date: order.date,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(VintedSalesCollection {
        sales,
        count,
        status: request.status,
        limit: request.limit,
        next_page: (request.page < pagination.total_pages).then(|| request.page + 1),
        pagination,
    })
}

fn decode_response(response: TransportResponse) -> Result<Value, AppError> {
    if !response.status.is_success() {
        return Err(status_error(response.status));
    }
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(invalid_response("response exceeded the size limit"));
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("response was not valid JSON"))
}

fn status_error(status: StatusCode) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "vinted_sales.authentication_required",
            "Vinted sales require a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: "flea vinted --portal fi auth login".to_owned(),
        });
        return error;
    }
    let mut error = AppError::upstream(
        "vinted_sales.upstream_failed",
        format!("Vinted sales returned HTTP {}", status.as_u16()),
    );
    error.upstream_transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    error.safe_to_retry = error.upstream_transient;
    error
}

fn invalid_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_sales.unexpected_response",
        "Vinted returned an unsupported sales response",
    )
    .with_details(json!({"reason": reason}))
}

fn transport_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        return invalid_response("response exceeded the size limit");
    }
    let mut result = AppError::upstream(
        "vinted_sales.transport_failed",
        "Vinted sales could not be reached",
    )
    .with_source(error);
    result.upstream_transient = true;
    result.safe_to_retry = true;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/vinted/sales/page-1.json"
        ))
        .unwrap()
    }

    #[test]
    fn request_encodes_only_sold_orders_with_explicit_pagination() {
        for status in [
            VintedSalesStatus::All,
            VintedSalesStatus::InProgress,
            VintedSalesStatus::Completed,
            VintedSalesStatus::Canceled,
        ] {
            let request = VintedSalesRequest {
                status,
                page: 7,
                limit: 20,
            };
            let url = endpoint(VINTED_FI_BINDING.portal_api_host, &request).unwrap();
            assert_eq!(
                url.as_str(),
                format!(
                    "https://www.vinted.fi/api/v2/my_orders?type=sold&status={}&page=7&per_page=20",
                    status.as_str()
                )
            );
        }
    }

    #[test]
    fn empty_history_and_out_of_range_pages_are_explicit() {
        for (page, entries, pages) in [(1, 0, 0), (1, 0, 1), (9, 3, 2)] {
            let raw = json!({"my_orders": [], "pagination": {"current_page": page, "total_entries": entries, "total_pages": pages}});
            let collection = normalize(
                raw,
                &VintedSalesRequest {
                    page,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(collection.count, 0);
            assert_eq!(collection.next_page, None);
            assert_eq!(collection.pagination.total_entries, entries);
        }
    }

    #[test]
    fn nullable_fields_and_unknown_status_are_not_inferred() {
        let collection = normalize(fixture(), &VintedSalesRequest::default()).unwrap();
        let sale = &collection.sales[1];
        assert_eq!(sale.transaction_id, None);
        assert_eq!(sale.conversation_id, None);
        assert_eq!(sale.title, None);
        assert_eq!(sale.price, None);
        assert_eq!(sale.status_text, None);
        assert_eq!(sale.photo_url, None);
        assert_eq!(sale.date, None);
        assert_eq!(
            sale.transaction_user_status.as_deref(),
            Some("future_status")
        );
    }

    #[test]
    fn malformed_orders_and_pagination_do_not_become_empty_success() {
        for (pointer, replacement) in [
            ("/my_orders", Value::Null),
            ("/my_orders", json!({})),
            ("/my_orders/0", json!(5)),
            ("/my_orders/0/transaction_id", json!({"secret": "private"})),
            ("/my_orders/0/title", json!({"secret": "private"})),
            ("/my_orders/0/price/amount", json!("private")),
            ("/my_orders/0/date", json!({"secret": "private"})),
            ("/pagination", Value::Null),
            ("/pagination", json!({})),
            ("/pagination/current_page", json!(2)),
            ("/pagination/total_entries", json!(1)),
            ("/pagination/total_pages", json!(-1)),
            ("/pagination/total_pages", json!(0)),
            ("/pagination/total_pages", json!("2")),
            ("/pagination/total_pages", json!(2147483648_u64)),
            ("/code", json!(99)),
        ] {
            let mut raw = fixture();
            *raw.pointer_mut(pointer).unwrap() = replacement;
            let error = normalize(raw, &VintedSalesRequest::default()).unwrap_err();
            assert_eq!(error.code, "vinted_sales.unexpected_response", "{pointer}");
            assert!(!format!("{error:?}").contains("private"));
        }
        assert!(normalize(json!({}), &VintedSalesRequest::default()).is_err());
        assert!(
            normalize(
                fixture(),
                &VintedSalesRequest {
                    limit: 1,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let raw = json!({"my_orders": [], "pagination": {"current_page": 1, "total_entries": 3, "total_pages": 2}});
        assert!(normalize(raw, &VintedSalesRequest::default()).is_err());
    }

    #[test]
    fn http_and_transport_errors_are_bounded_retryable_and_redacted() {
        for (status, exit, retry) in [
            (401, 10, false),
            (403, 10, false),
            (404, 40, false),
            (429, 40, true),
            (500, 40, true),
        ] {
            let error = decode_response(TransportResponse {
                status: StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: b"private-response".to_vec(),
            })
            .unwrap_err();
            assert_eq!(error.exit_class.code(), exit);
            assert_eq!(error.safe_to_retry, retry);
            assert!(!format!("{error:?}").contains("private-response"));
        }
        for body in [
            b"not json private-response".to_vec(),
            vec![b' '; MAX_RESPONSE_BYTES + 1],
        ] {
            let error = decode_response(TransportResponse {
                status: StatusCode::OK,
                headers: Default::default(),
                body,
            })
            .unwrap_err();
            assert_eq!(error.code, "vinted_sales.unexpected_response");
            assert!(!format!("{error:?}").contains("private-response"));
        }
        assert!(
            !transport_error(TransportError::request(
                TransportErrorKind::ResponseTooLarge
            ))
            .safe_to_retry
        );
        assert!(
            transport_error(TransportError::request(TransportErrorKind::Timeout)).safe_to_retry
        );
    }
}
