use std::{collections::HashSet, sync::Arc};

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;

use super::client::{HttpFailure, RequestSpec, ToriClient, compatibility};
use crate::{
    domain::{
        envelope::NextAction,
        tori_sale::{ToriSale, ToriSalesCollection},
    },
    error::AppError,
    retry::{FailureKind, OperationMethod, RetryContext, classify},
};

const SOLD_FACET: &str = "DISPOSED";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToriSalesRequest {
    pub offset: usize,
    pub limit: usize,
}

impl ToriSalesRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        // The native summary service takes signed 32-bit pagination arguments.
        if self.offset > i32::MAX as usize || self.limit == 0 || self.limit > i32::MAX as usize {
            return Err(AppError::usage(
                "sales offset must be from 0 through 2147483647 and limit from 1 through 2147483647",
            ));
        }
        Ok(())
    }
}

pub struct ToriSales {
    client: Arc<dyn ToriClient>,
}

impl ToriSales {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    pub async fn list(&self, request: ToriSalesRequest) -> Result<ToriSalesCollection, AppError> {
        request.validate()?;
        let response = self
            .client
            .execute(RequestSpec::new(
                Method::GET,
                format!(
                    "/search?facet={SOLD_FACET}&offset={}&limit={}",
                    request.offset, request.limit
                ),
                compatibility::SERVICE_AD_SUMMARIES,
            ))
            .await
            .map_err(|error| {
                let failure = HttpFailure::from(error);
                AppError::upstream("tori_sales.request_failed", "the Tori sales request failed")
                    .retry_classification(
                        failure.retry_classification(RetryContext::read(OperationMethod::Get)),
                    )
            })?;
        if !response.status.is_success() {
            return Err(status_error(response.status));
        }
        let page = serde_json::from_slice(&response.body)
            .map_err(|_| invalid_response("summaries or pagination had an unsupported shape"))?;
        normalize(page, &request)
    }
}

// The summary projection requires identity and pagination, allows nullable
// display text, and never decodes actions or account statistics.
#[derive(Deserialize)]
struct SummaryPage {
    summaries: Vec<Summary>,
    total: usize,
    query: SummaryQuery,
}

#[derive(Deserialize)]
struct SummaryQuery {
    facet: String,
    offset: usize,
    limit: usize,
}

#[derive(Deserialize)]
struct Summary {
    id: u64,
    state: SummaryState,
    #[serde(deserialize_with = "Option::deserialize")]
    data: Option<SummaryData>,
}

#[derive(Deserialize)]
struct SummaryState {
    #[serde(rename = "type")]
    state_type: String,
}

#[derive(Deserialize)]
struct SummaryData {
    title: Option<String>,
    subtitle: Option<String>,
    image: Option<String>,
}

fn normalize(
    page: SummaryPage,
    request: &ToriSalesRequest,
) -> Result<ToriSalesCollection, AppError> {
    let count = page.summaries.len();
    let end = request
        .offset
        .checked_add(count)
        .ok_or_else(|| invalid_response("pagination overflowed"))?;
    if page.query.facet != SOLD_FACET
        || page.query.offset != request.offset
        || page.query.limit != request.limit
        || page.total > i32::MAX as usize
        || count > request.limit
        || (count > 0 && end > page.total)
        || (count == 0 && request.offset < page.total)
    {
        return Err(invalid_response(
            "pagination was inconsistent with the requested page",
        ));
    }
    let mut seen = HashSet::new();
    let sales = page
        .summaries
        .into_iter()
        .map(|summary| {
            if summary.id == 0
                || summary.id > i64::MAX as u64
                || !seen.insert(summary.id)
                || summary.state.state_type.trim().is_empty()
            {
                return Err(invalid_response("summary identity or state was invalid"));
            }
            let (title, subtitle, image) = match summary.data {
                Some(data) => (data.title, data.subtitle, data.image),
                None => (None, None, None),
            };
            Ok(ToriSale {
                listing_id: summary.id.to_string(),
                title,
                state: summary.state.state_type,
                subtitle,
                image,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(ToriSalesCollection {
        sales,
        count,
        total: page.total,
        offset: request.offset,
        limit: request.limit,
        next_offset: (end < page.total).then_some(end),
    })
}

fn status_error(status: StatusCode) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "tori_sales.authentication_required",
            "Tori sales require a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: "flea tori auth login".to_owned(),
        });
        return error;
    }
    AppError::upstream(
        "tori_sales.upstream_failed",
        format!("Tori sales returned HTTP {}", status.as_u16()),
    )
    .retry_classification(classify(
        FailureKind::HttpStatus(status.as_u16()),
        RetryContext::read(OperationMethod::Get),
    ))
}

fn invalid_response(reason: &str) -> AppError {
    AppError::upstream(
        "tori_sales.unexpected_response",
        "Tori returned an unsupported sales response",
    )
    .with_details(json!({"reason": reason}))
}
