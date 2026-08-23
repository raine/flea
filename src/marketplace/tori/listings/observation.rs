use std::{ops::ControlFlow, time::Duration};

use serde_json::{Value, json};

use super::scan::{
    CollectionPage, CollectionPageSource, CollectionScan, CollectionScanError, scan_collection,
};
use crate::{
    domain::observation::{Observation, ObservationOperation, ObservationSource},
    marketplace::tori::{
        adinput::{ApiError, HttpRequest, HttpResponse, HttpTransport, RetryPolicy},
        client::compatibility,
    },
};

const DRAFT_ACTION_ATTEMPTS: usize = 6;

pub(crate) struct ListingObservations<'a, T> {
    transport: &'a T,
}

impl<'a, T: HttpTransport> ListingObservations<'a, T> {
    pub(crate) const fn new(transport: &'a T) -> Self {
        Self { transport }
    }

    pub(crate) async fn find_summary(&self, listing_id: &str) -> Result<Option<Value>, ApiError> {
        let source = ObservationPages {
            transport: self.transport,
            facet: None,
            model: CollectionModel::Listings,
        };
        let scan = scan_collection(&source, async |summaries, _| {
            summaries
                .into_iter()
                .find(|summary| listing_value_id_matches(summary.get("id"), listing_id))
                .map(|summary| ControlFlow::Break(normalize_observed_summary(&summary, listing_id)))
                .unwrap_or(ControlFlow::Continue(()))
        })
        .await
        .map_err(|error| scan_error(error, CollectionModel::Listings))?;

        Ok(match scan {
            CollectionScan::Match(summary) => Some(summary),
            CollectionScan::Complete { .. } => None,
        })
    }

    pub(crate) async fn draft_delete_action(&self, draft_id: &str) -> Result<String, ApiError> {
        for action_attempt in 0..DRAFT_ACTION_ATTEMPTS {
            let source = ObservationPages {
                transport: self.transport,
                facet: Some("DRAFT"),
                model: CollectionModel::Drafts,
            };
            let scan = scan_collection(&source, async |summaries, status| {
                let Some(summary) = summaries
                    .into_iter()
                    .find(|summary| listing_value_id_matches(summary.get("id"), draft_id))
                else {
                    return ControlFlow::Continue(());
                };
                ControlFlow::Break(draft_delete_path(&summary, status))
            })
            .await;

            match scan {
                Ok(CollectionScan::Match(path)) => return path,
                Ok(CollectionScan::Complete { .. })
                | Err(CollectionScanError::PageBoundExceeded) => {}
                Err(error) => return Err(scan_error(error, CollectionModel::Drafts)),
            }
            if action_attempt + 1 < DRAFT_ACTION_ATTEMPTS {
                let delay_ms = 250 * (1_u64 << action_attempt);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
        Err(listing_observation_model_error(
            200,
            "draft_delete_action_unavailable",
        ))
    }
}

#[derive(Clone, Copy)]
enum CollectionModel {
    Listings,
    Drafts,
}

struct ObservationPages<'a, T> {
    transport: &'a T,
    facet: Option<&'static str>,
    model: CollectionModel,
}

impl<T: HttpTransport> CollectionPageSource for ObservationPages<'_, T> {
    type Item = Value;
    type Metadata = u16;
    type Error = ApiError;

    async fn page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<CollectionPage<Self::Item, Self::Metadata>, Self::Error> {
        let path = match self.facet {
            Some(facet) => format!("/search?facet={facet}&limit={limit}&offset={offset}"),
            None => format!("/search?limit={limit}&offset={offset}"),
        };
        let mut request =
            HttpRequest::read(ObservationSource::AuthenticatedListingCollection, path);
        if self.facet.is_some() {
            request = request.with_service(compatibility::SERVICE_AD_SUMMARIES);
        }
        let response = execute(self.transport, request).await?;
        if response.body_is_unparseable && matches!(self.model, CollectionModel::Listings) {
            return Err(listing_observation_model_error(
                response.status,
                "collection_unparseable",
            ));
        }
        let summaries = response
            .body
            .get("summaries")
            .or_else(|| response.body.get("listings"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                listing_observation_model_error(response.status, self.model.unrecognized())
            })?;
        let total = response
            .body
            .get("total")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                listing_observation_model_error(response.status, self.model.invalid_total())
            })?;
        Ok(CollectionPage {
            items: summaries,
            total,
            metadata: response.status,
            status: Some(response.status),
        })
    }
}

impl CollectionModel {
    const fn unrecognized(self) -> &'static str {
        match self {
            Self::Listings => "collection_unrecognized",
            Self::Drafts => "draft_collection_unrecognized",
        }
    }

    const fn invalid_total(self) -> &'static str {
        match self {
            Self::Listings => "collection_total_invalid",
            Self::Drafts => "draft_collection_total_invalid",
        }
    }

    const fn changed_total(self) -> &'static str {
        match self {
            Self::Listings => "collection_total_changed",
            Self::Drafts => "draft_collection_total_changed",
        }
    }

    const fn incomplete(self) -> &'static str {
        match self {
            Self::Listings => "collection_pagination_incomplete",
            Self::Drafts => "draft_collection_pagination_incomplete",
        }
    }
}

async fn execute<T: HttpTransport>(
    transport: &T,
    request: HttpRequest,
) -> Result<HttpResponse, ApiError> {
    debug_assert!(request.retry == RetryPolicy::BoundedRead);
    let retry_context = request.retry_context();
    let source = request.observation_source;
    let response = transport.execute(request).await?;
    if (200..300).contains(&response.status) {
        Ok(response)
    } else {
        Err(ApiError::response(&response, retry_context, source))
    }
}

fn scan_error(error: CollectionScanError<ApiError>, model: CollectionModel) -> ApiError {
    match error {
        CollectionScanError::Source(error) => error,
        CollectionScanError::TotalChanged { status } => {
            listing_observation_model_error(status.unwrap_or(200), model.changed_total())
        }
        CollectionScanError::PrematureEmptyPage { status } => {
            listing_observation_model_error(status.unwrap_or(200), model.incomplete())
        }
        CollectionScanError::PageBoundExceeded => {
            listing_observation_model_error(200, "collection_pagination_bounded")
        }
    }
}

fn draft_delete_path(summary: &Value, status: u16) -> Result<String, ApiError> {
    let action = summary
        .get("actions")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions.iter().find(|action| {
                action
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("DELETE"))
                    && action
                        .get("method")
                        .or_else(|| action.get("httpMethod"))
                        .and_then(Value::as_str)
                        .is_some_and(|method| method.eq_ignore_ascii_case("DELETE"))
            })
        })
        .ok_or_else(|| listing_observation_model_error(status, "draft_delete_action_missing"))?;
    let path = action
        .get("path")
        .or_else(|| action.get("urlPath"))
        .or_else(|| action.get("url_path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            listing_observation_model_error(status, "draft_delete_action_path_invalid")
        })?;
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['#', '\\'])
        || path.chars().any(char::is_control)
    {
        return Err(listing_observation_model_error(
            status,
            "draft_delete_action_path_invalid",
        ));
    }
    Ok(path.to_owned())
}

fn listing_value_id_matches(value: Option<&Value>, listing_id: &str) -> bool {
    match value {
        Some(Value::String(value)) => value == listing_id,
        Some(Value::Number(value)) => value.to_string() == listing_id,
        _ => false,
    }
}

fn normalize_observed_summary(summary: &Value, listing_id: &str) -> Value {
    let data = summary.get("data").unwrap_or(&Value::Null);
    json!({
        "listing_id": listing_id,
        "title": data.get("title"),
        "price": data.get("subtitle"),
        "state": summary.get("state"),
        "location": data.get("location").or_else(|| data.get("area")).or_else(|| data.get("place")),
        "image_url": data.get("image"),
        "public_url": format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}"),
        "observation_source": "collection",
    })
}

fn listing_observation_model_error(status: u16, model: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unrecognized_model",
        "Tori returned an unrecognized listing collection model",
    )
    .with_observation(
        Observation::unrecognized_response(
            ObservationSource::AuthenticatedListingCollection,
            Some(status),
        ),
        ObservationOperation::Read,
    );
    error.status = Some(status);
    error.details = Some(Box::new(json!({
        "status": status,
        "response_model": model,
    })));
    error
}
