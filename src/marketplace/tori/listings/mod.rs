mod gateway;
mod normalization;
pub(crate) mod observation;
mod scan;
mod taxonomy;

pub use gateway::{
    HttpListingsApi, ListingsApi, ListingsApiError, UpstreamAction, UpstreamFacet, UpstreamListing,
    UpstreamListingPage, UpstreamListingSummary, UpstreamState, UpstreamStatistic,
    UpstreamStatistics, UpstreamSummaryData,
};
pub use taxonomy::{
    CATEGORY_SEARCH_LIMIT_DEFAULT, CATEGORY_SEARCH_LIMIT_MAX, CategoryRequest, CategoryResult,
    CategorySearchOptions, Taxonomy, TaxonomyApi, UpstreamCategory, UpstreamCategoryTaxonomy,
};

use std::{
    collections::{BTreeMap, HashSet},
    ops::ControlFlow,
};

use normalization::{
    collect_image_urls, commerce_from_fields, detail_observation_status, normalize_facet,
    normalize_listing_detail_for_id, normalize_listing_for_id, normalize_summary, summary_detail,
    summary_id, value_id,
};
use scan::{
    COLLECTION_PAGE_SIZE, CollectionPage, CollectionPageSource, CollectionScan,
    CollectionScanError, scan_collection,
};

use serde_json::{Value, json};

use crate::{
    domain::{
        commerce::{Price, TradeType},
        listing::{
            ListingCollection, ListingCopySource, ListingDetail, ListingMutation, ListingRef,
            ListingSnapshot, ListingState, ListingSummary,
        },
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
    retry::{FailureKind, OperationMethod, RetryContext, classify},
};

pub const LISTING_PAGE_SIZE: usize = COLLECTION_PAGE_SIZE;

struct ListingCollectionSource<'a> {
    api: &'a dyn ListingsApi,
}

impl CollectionPageSource for ListingCollectionSource<'_> {
    type Item = UpstreamListingSummary;
    type Metadata = Vec<UpstreamFacet>;
    type Error = ListingsApiError;

    async fn page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<CollectionPage<Self::Item, Self::Metadata>, Self::Error> {
        self.api
            .listing_page(offset, limit)
            .await
            .map(|page| CollectionPage {
                items: page.summaries,
                total: page.total,
                metadata: page.facets,
                status: None,
            })
    }
}

pub struct Listings<'a> {
    api: &'a dyn ListingsApi,
}

impl<'a> Listings<'a> {
    pub fn new(api: &'a dyn ListingsApi) -> Self {
        Self { api }
    }

    pub async fn list(&self) -> Result<ListingCollection, AppError> {
        let source = ListingCollectionSource { api: self.api };
        let mut listings = Vec::new();
        let mut facets = Vec::new();
        let mut seen = HashSet::new();
        let scan = scan_collection(&source, async |summaries, page_facets| {
            if facets.is_empty() {
                facets = page_facets.into_iter().map(normalize_facet).collect();
            }
            for summary in summaries {
                let listing_id = match value_id(&summary.id) {
                    Ok(listing_id) => listing_id,
                    Err(error) => return ControlFlow::Break(Err(error)),
                };
                let detail = match self.api.listing(&listing_id).await {
                    Ok(detail) => detail,
                    Err(error) => {
                        return ControlFlow::Break(Err(listing_error(
                            error,
                            Some(&listing_id),
                            RetryContext::read(OperationMethod::Get),
                        )));
                    }
                };
                match value_id(&detail.id) {
                    Ok(detail_id) if detail_id == listing_id => {}
                    Ok(_) => {
                        return ControlFlow::Break(Err(unexpected(
                            "listing detail returned a different ID",
                        )));
                    }
                    Err(error) => return ControlFlow::Break(Err(error)),
                }
                let (trade_type, price) = commerce_from_fields(&detail.fields);
                let normalized = match normalize_summary(summary, trade_type, price) {
                    Ok(normalized) => normalized,
                    Err(error) => return ControlFlow::Break(Err(error)),
                };
                if !seen.insert(normalized.listing_id.clone()) {
                    return ControlFlow::Break(Err(unexpected(
                        "listing pagination returned a duplicate item",
                    )));
                }
                listings.push(normalized);
            }
            ControlFlow::Continue(())
        })
        .await
        .map_err(list_scan_error)?;
        let total = match scan {
            CollectionScan::Complete { total } => total,
            CollectionScan::Match(Err(error)) => return Err(error),
            CollectionScan::Match(Ok(())) => unreachable!("listing scans stop only on errors"),
        };

        Ok(ListingCollection {
            listings,
            total: total as u64,
            facets,
        })
    }

    pub async fn show(&self, listing_id: &str) -> Result<ListingDetail, AppError> {
        validate_id(listing_id)?;
        match self.api.listing(listing_id).await {
            Ok(listing) => match normalize_listing_detail_for_id(listing, listing_id) {
                Ok(detail) => Ok(detail),
                Err(detail_error) => match self.find_summary(listing_id).await {
                    Ok(Some(summary)) => Ok(summary_detail(summary)),
                    Ok(None) | Err(_) => Err(detail_error),
                },
            },
            Err(detail_error) => {
                let summary = self.find_summary(listing_id).await;
                match summary {
                    Ok(Some(summary)) => Ok(summary_detail(summary)),
                    Ok(None) => Err(listing_error(
                        detail_error,
                        Some(listing_id),
                        RetryContext::read(OperationMethod::Get),
                    )),
                    Err(collection_error) => {
                        let classification = listing_error(
                            collection_error,
                            Some(listing_id),
                            RetryContext::read(OperationMethod::Get),
                        );
                        let observation = if classification.upstream_transient {
                            Observation::temporarily_unavailable(
                                "listing_reconciliation",
                                None,
                                false,
                            )
                        } else {
                            Observation::unrecognized_response("listing_reconciliation", None)
                        };
                        let mut error = AppError::upstream(
                            "listing.observation_delayed",
                            "listing identity could not be reconciled between detail and collection observations",
                        )
                        .with_observation(observation, ObservationOperation::Read);
                        error.details = Some(Box::new(json!({
                            "listing_id": listing_id,
                            "detail_status": detail_observation_status(&detail_error),
                            "collection_status": "unavailable",
                            "observation_attempts": 2,
                        })));
                        error
                            .next_actions
                            .push(crate::domain::envelope::NextAction {
                                command: format!("flea tori listing show {listing_id}"),
                            });
                        Err(error)
                    }
                }
            }
        }
    }

    async fn find_summary(
        &self,
        listing_id: &str,
    ) -> Result<Option<ListingSummary>, ListingsApiError> {
        let source = ListingCollectionSource { api: self.api };
        let scan = scan_collection(&source, async |summaries, _| {
            for summary in summaries {
                match summary_id(&summary.id) {
                    Ok(id) if id == listing_id => return ControlFlow::Break(Ok(summary)),
                    Ok(_) => {}
                    Err(error) => return ControlFlow::Break(Err(error)),
                }
            }
            ControlFlow::Continue(())
        })
        .await
        .map_err(reconciliation_scan_error)?;

        match scan {
            CollectionScan::Complete { .. } => Ok(None),
            CollectionScan::Match(Err(error)) => Err(error),
            CollectionScan::Match(Ok(summary)) => {
                normalize_summary(summary, TradeType::Unknown, Price::unavailable(None))
                    .map(Some)
                    .map_err(|_| {
                        ListingsApiError::UnexpectedResponse(
                            "matching listing summary was malformed".to_owned(),
                        )
                    })
            }
        }
    }

    pub async fn snapshot(&self, listing_id: &str) -> Result<ListingSnapshot, AppError> {
        validate_id(listing_id)?;
        self.api
            .listing(listing_id)
            .await
            .map_err(|error| {
                listing_error(
                    error,
                    Some(listing_id),
                    RetryContext::read(OperationMethod::Get),
                )
            })
            .and_then(|listing| normalize_listing_for_id(listing, listing_id))
    }

    pub async fn update(
        &self,
        listing_id: &str,
        changes: BTreeMap<String, Value>,
    ) -> Result<ListingDetail, AppError> {
        validate_id(listing_id)?;
        if changes.is_empty() {
            return Err(AppError::usage(
                "listing update requires at least one field",
            ));
        }
        validate_changes(&changes)?;

        let snapshot = self.snapshot(listing_id).await?;
        let mut complete_fields = snapshot.source_fields;
        complete_fields.extend(changes);
        match self
            .api
            .update_listing(listing_id, &snapshot.etag, &complete_fields)
            .await
        {
            Ok(listing) => {
                normalize_listing_for_id(listing, listing_id).map(|snapshot| snapshot.detail)
            }
            Err(ListingsApiError::Conflict) => {
                let fresh = self
                    .snapshot(listing_id)
                    .await
                    .ok()
                    .map(|snapshot| snapshot.detail);
                let mut retry_context = RetryContext::mutation(OperationMethod::Put).with_etag();
                if fresh.is_some() {
                    retry_context = retry_context.with_authoritative_observation();
                }
                let classification = classify(FailureKind::PreconditionFailed, retry_context);
                let mut error = AppError::new(
                    "listing.conflict",
                    "listing changed remotely; no fields were overwritten",
                    ExitClass::Conflict,
                )
                .retry_classification(classification);
                error.details = Some(Box::new(json!({
                    "listing_id": listing_id,
                    "current": fresh,
                })));
                error
                    .next_actions
                    .push(crate::domain::envelope::NextAction {
                        command: format!("flea tori listing show {listing_id}"),
                    });
                Err(error)
            }
            Err(error) => Err(listing_mutation_error(error, listing_id, "update")),
        }
    }

    pub async fn dispose(&self, listing_id: &str) -> Result<ListingMutation, AppError> {
        validate_id(listing_id)?;
        self.api
            .dispose_listing(listing_id)
            .await
            .map_err(|error| listing_mutation_error(error, listing_id, "dispose"))?;
        Ok(ListingMutation {
            listing_id: listing_id.to_owned(),
            state: ListingState::Disposed,
        })
    }

    pub async fn delete(&self, listing_id: &str) -> Result<ListingRef, AppError> {
        validate_id(listing_id)?;
        self.api
            .delete_listing(listing_id)
            .await
            .map_err(|error| listing_mutation_error(error, listing_id, "delete"))?;
        Ok(ListingRef {
            listing_id: listing_id.to_owned(),
        })
    }

    pub async fn copy_source(&self, listing_id: &str) -> Result<ListingCopySource, AppError> {
        let snapshot = self.snapshot(listing_id).await?;
        let mut fields = snapshot.source_fields;
        let mut image_urls = Vec::new();
        for key in ["image", "images", "multi_image"] {
            if let Some(value) = fields.remove(key) {
                collect_image_urls(&value, &mut image_urls);
            }
        }
        image_urls.dedup();

        Ok(ListingCopySource {
            listing_id: listing_id.to_owned(),
            fields,
            image_urls,
        })
    }
}

fn validate_changes(changes: &BTreeMap<String, Value>) -> Result<(), AppError> {
    if let Some(value) = changes.get("trade_type") {
        let valid = matches!(value.as_str(), Some("sell" | "give_away" | "wanted"));
        if !valid {
            return Err(validation_error(
                "trade_type",
                "expected one of: sell, give_away, wanted",
            ));
        }
    }
    if let Some(value) = changes.get("price")
        && value
            .as_f64()
            .is_none_or(|price| !price.is_finite() || price < 0.0)
    {
        return Err(validation_error("price", "expected a non-negative number"));
    }
    for key in ["category", "title", "description", "postal_code"] {
        if let Some(value) = changes.get(key) {
            let Some(value) = value.as_str() else {
                return Err(validation_error(key, "expected a string"));
            };
            if value.trim().is_empty() {
                return Err(validation_error(key, "must not be empty"));
            }
        }
    }
    if let Some(value) = changes.get("delivery") {
        let values = match value {
            Value::String(value) => vec![value.as_str()],
            Value::Array(values) => values
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| validation_error("delivery", "expected semantic string values"))?,
            _ => {
                return Err(validation_error(
                    "delivery",
                    "expected semantic string values",
                ));
            }
        };
        if values.is_empty() || values.iter().any(|value| !is_semantic_value(value)) {
            return Err(validation_error(
                "delivery",
                "expected non-empty lowercase machine values",
            ));
        }
    }
    Ok(())
}

fn is_semantic_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_id(listing_id: &str) -> Result<(), AppError> {
    if listing_id.is_empty()
        || listing_id.len() > 128
        || !listing_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(AppError::usage("listing ID is invalid"))
    } else {
        Ok(())
    }
}

fn validation_error(field: &str, message: &str) -> AppError {
    let mut error = AppError::new(
        "listing.validation_failed",
        "listing fields are invalid",
        ExitClass::Validation,
    );
    error.details = Some(Box::new(json!({ "fields": { field: message } })));
    error
}

fn listing_error(
    error: ListingsApiError,
    listing_id: Option<&str>,
    context: RetryContext,
) -> AppError {
    let mut app_error = match error {
        ListingsApiError::Authentication => AppError::authentication(
            "auth.rejected",
            "Tori rejected authentication for the listing request",
        ),
        ListingsApiError::NotFound => resource_not_found(
            "listing.not_found",
            "listing",
            listing_id.unwrap_or_default(),
        ),
        ListingsApiError::Conflict => AppError::new(
            "listing.conflict",
            "listing changed remotely",
            ExitClass::Conflict,
        ),
        ListingsApiError::Validation { fields, .. } => {
            let mut error = AppError::new(
                "listing.validation_failed",
                "listing validation failed",
                ExitClass::Validation,
            );
            error.details = Some(Box::new(json!({
                "listing_id": listing_id,
                "fields": fields
            })));
            error
        }
        other => upstream_error(other, context),
    };
    if let Some(listing_id) = listing_id
        && app_error.observation.is_some()
    {
        let command = match app_error
            .observation
            .as_ref()
            .map(|observation| observation.state)
        {
            Some(crate::domain::observation::ObservationState::TemporarilyUnavailable) => {
                format!("flea tori listing show {listing_id}")
            }
            _ => "flea tori listing list".to_owned(),
        };
        app_error
            .next_actions
            .push(crate::domain::envelope::NextAction { command });
    }
    app_error
}

pub(super) fn resource_not_found(code: &str, resource: &str, id: &str) -> AppError {
    let source = match resource {
        "listing" => "listing_detail",
        "category" => "category_taxonomy",
        _ => "remote_resource",
    };
    AppError::new(
        code,
        format!("{resource} was not found"),
        ExitClass::Validation,
    )
    .with_details(json!({ format!("{resource}_id"): id }))
    .with_observation(
        Observation::confirmed_absent(source, Some(404)),
        ObservationOperation::Read,
    )
}

fn listing_mutation_error(error: ListingsApiError, listing_id: &str, operation: &str) -> AppError {
    let outcome_uncertain = matches!(
        &error,
        ListingsApiError::Transport
            | ListingsApiError::UnexpectedResponse(_)
            | ListingsApiError::Upstream(408 | 425 | 429 | 500 | 502 | 503 | 504)
    );
    let method = match operation {
        "update" => OperationMethod::Put,
        "delete" => OperationMethod::Delete,
        _ => OperationMethod::Post,
    };
    let mut app_error = listing_error(error, Some(listing_id), RetryContext::mutation(method));
    if app_error.exit_class != ExitClass::Upstream {
        if app_error.exit_class == ExitClass::Conflict {
            app_error
                .next_actions
                .push(crate::domain::envelope::NextAction {
                    command: format!("flea tori listing show {listing_id}"),
                });
        }
        return app_error;
    }
    if outcome_uncertain {
        app_error.code = "mutation.uncertain".to_owned();
        app_error.message =
            "The upstream failure may be temporary, but the listing mutation outcome is unknown"
                .to_owned();
    }
    let mut app_error = app_error.with_partial(json!({
        "listing_id": listing_id,
        "operation": operation,
    }));
    app_error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: format!("flea tori listing show {listing_id}"),
        });
    app_error
}

pub(super) fn upstream_error(error: ListingsApiError, context: RetryContext) -> AppError {
    let unrecognized_model = matches!(error, ListingsApiError::UnexpectedResponse(_));
    let operation = if context.method.is_read() {
        ObservationOperation::Read
    } else {
        ObservationOperation::Mutation
    };
    let (code, message, observation) = match error {
        ListingsApiError::Transport => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::temporarily_unavailable("listing_service", None, false),
        ),
        ListingsApiError::Upstream(status) if crate::retry::is_transient_status(status) => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::temporarily_unavailable("listing_service", Some(status), true),
        ),
        ListingsApiError::Upstream(status) => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::unrecognized_response("listing_service", Some(status)),
        ),
        ListingsApiError::UnexpectedResponse(_) => (
            "upstream.unexpected_response",
            "Tori returned an unexpected listing response",
            Observation::unrecognized_response("listing_service", Some(200)),
        ),
        _ => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::unrecognized_response("listing_service", None),
        ),
    };
    let mut error =
        AppError::new(code, message, ExitClass::Upstream).with_observation(observation, operation);
    if unrecognized_model {
        error.details = Some(Box::new(json!({
            "response_model": "unrecognized",
            "response_status_class": "success",
        })));
    }
    error
}

fn list_scan_error(error: CollectionScanError<ListingsApiError>) -> AppError {
    match error {
        CollectionScanError::Source(error) => {
            listing_error(error, None, RetryContext::read(OperationMethod::Get))
        }
        CollectionScanError::TotalChanged { .. } => {
            unexpected("listing total changed during pagination")
        }
        CollectionScanError::PrematureEmptyPage { .. } => {
            unexpected("listing pagination ended before the reported total")
        }
        CollectionScanError::PageBoundExceeded => {
            unexpected("listing pagination exceeded its safety bound")
        }
    }
}

fn reconciliation_scan_error(error: CollectionScanError<ListingsApiError>) -> ListingsApiError {
    match error {
        CollectionScanError::Source(error) => error,
        CollectionScanError::TotalChanged { .. } => ListingsApiError::UnexpectedResponse(
            "listing total changed during identity reconciliation".to_owned(),
        ),
        CollectionScanError::PrematureEmptyPage { .. } => ListingsApiError::UnexpectedResponse(
            "listing pagination ended before the reported total".to_owned(),
        ),
        CollectionScanError::PageBoundExceeded => ListingsApiError::UnexpectedResponse(
            "listing identity reconciliation exceeded its safety bound".to_owned(),
        ),
    }
}

pub(super) fn unexpected(message: &str) -> AppError {
    upstream_error(
        ListingsApiError::UnexpectedResponse(message.to_owned()),
        RetryContext::read(OperationMethod::Get),
    )
}
