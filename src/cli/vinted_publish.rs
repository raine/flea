use std::{fs, io::Read, path::PathBuf, time::Duration};

use clap::{Args, Subcommand};
use serde_json::json;
use tokio::time::Instant;

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    domain::{
        envelope::{NextAction, Warning},
        vinted_listing::VintedListingState,
    },
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            auth::VintedCredentialRecord,
            brand::validate_listing_brand,
            composer::{VintedComposer, VintedPublicationComposer},
            draft::{DEFAULT_PAGE_SIZE, DraftListRequest, VintedDraftApi, VintedDrafts},
            listing::{
                VintedListingApi, VintedListingRequest, VintedListingResult, VintedListings,
            },
            publication::{
                ListingInput, PublicationOperation, PublicationResult,
                PublicationVerificationStatus, VintedPublication, VintedPublicationApi,
                confirmed_publication_result, review_pending_item_id,
            },
            publication_discovery::VintedPublicationDiscoveryApi,
            readiness::VintedReadinessApi,
            search::VintedSearchSession,
            semantic_values::has_semantic_values,
        },
    },
};

const PUBLICATION_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLICATION_VERIFICATION_INTERVAL: Duration = Duration::from_secs(1);
const PUBLICATION_VERIFICATION_ATTEMPTS: usize = 5;

#[derive(Clone, Copy)]
struct PublicationVerificationConfig {
    timeout: Duration,
    interval: Duration,
    attempts: usize,
}

impl Default for PublicationVerificationConfig {
    fn default() -> Self {
        Self {
            timeout: PUBLICATION_VERIFICATION_TIMEOUT,
            interval: PUBLICATION_VERIFICATION_INTERVAL,
            attempts: PUBLICATION_VERIFICATION_ATTEMPTS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationVerificationOutcome {
    Public,
    Moderated,
    TimedOut,
}

#[derive(Debug, Args)]
pub struct VintedDraftArgs {
    #[command(subcommand)]
    pub command: VintedDraftCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedDraftCommand {
    #[command(
        about = "List the authenticated account's Vinted drafts",
        long_about = "Fetch one bounded page of remote Vinted drafts in newest-first order with stable IDs and concise summaries."
    )]
    List {
        /// One-based page number.
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Drafts per page, from 1 through 100.
        #[arg(long, default_value_t = DEFAULT_PAGE_SIZE)]
        limit: u16,
    },
    #[command(
        about = "Show complete remote Vinted draft state",
        long_about = "Fetch the authoritative editable draft state, including assigned photo IDs and display order, category, attributes, brand, colors, price, package, and revision metadata when available."
    )]
    Show {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Validate Vinted draft publication readiness",
        long_about = "Fetch authoritative remote draft state and report field-level local schema blockers, deterministic upstream validation errors, and account prerequisites without changing the draft."
    )]
    Validate {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Create a Vinted draft from a complete listing input",
        long_about = "Sanitize and upload images in argument order, then create a Vinted draft from a complete JSON payload whose category, attributes, price, and package values were discovered at runtime."
    )]
    Create(PublicationInputArgs),
    #[command(
        about = "Replace a Vinted draft from a complete listing input",
        long_about = "Sanitize and upload the complete image set, then replace a Vinted draft using a complete JSON payload. Unspecified remote values are not preserved."
    )]
    Update {
        /// Numeric Vinted draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: PublicationInputArgs,
    },
    #[command(
        about = "Publish a Vinted draft from a complete listing input",
        long_about = "Reuse the draft's verified remote photos by default, or replace the complete photo set when --image is passed, then complete the draft using a complete runtime-discovered JSON payload. Confirmed review-pending publications receive bounded read-only account verification."
    )]
    Publish {
        /// Numeric Vinted draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: DraftCompletionInputArgs,
    },
    #[command(
        about = "Delete a Vinted draft",
        long_about = "Permanently delete the selected Vinted draft without uploading images or changing a public listing."
    )]
    Delete {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
}

impl VintedDraftCommand {
    pub const fn telemetry_name(&self) -> &'static str {
        match self {
            Self::List { .. } => "draft list",
            Self::Show { .. } => "draft show",
            Self::Validate { .. } => "draft validate",
            Self::Create(_) => "draft create",
            Self::Update { .. } => "draft update",
            Self::Publish { .. } => "draft publish",
            Self::Delete { .. } => "draft delete",
        }
    }
}

#[derive(Debug, Args)]
pub struct PublicationInputArgs {
    /// Complete Vinted listing JSON, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// JPEG, PNG, HEIC, or HEIF image in final display order.
    #[arg(long, value_name = "PATH", required = true)]
    pub image: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DraftCompletionInputArgs {
    /// Complete Vinted listing JSON, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// Replace all remote photos with these images in final display order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
}

pub async fn execute_readiness(
    portal: PortalId,
    session: &dyn VintedSearchSession,
    readiness_api: &dyn VintedReadinessApi,
) -> Result<CommandOutcome, AppError> {
    let credentials = session.credentials(portal).await?;
    let result = readiness_api.readiness(&credentials).await?;
    Ok(CommandOutcome::new(
        CommandData::VintedPublicationReadiness(result),
    ))
}

pub async fn execute_direct(
    portal: PortalId,
    args: PublicationInputArgs,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
    listing_api: &dyn VintedListingApi,
) -> Result<CommandOutcome, AppError> {
    execute_operation(
        portal,
        PublicationOperation::Publish,
        Some(args),
        session,
        api,
        discovery_api,
        listing_api,
    )
    .await
}

pub async fn execute_draft(
    portal: PortalId,
    command: VintedDraftCommand,
    session: &dyn VintedSearchSession,
    publication_api: &dyn VintedPublicationApi,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
    draft_api: &dyn VintedDraftApi,
    listing_api: &dyn VintedListingApi,
) -> Result<CommandOutcome, AppError> {
    match command {
        VintedDraftCommand::List { page, limit } => {
            let result = VintedDrafts::new(session, draft_api)
                .list(
                    portal,
                    DraftListRequest {
                        page,
                        per_page: limit,
                    },
                )
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraftCollection(
                result,
            )));
        }
        VintedDraftCommand::Show { draft_id } => {
            let result = VintedDrafts::new(session, draft_api)
                .show(portal, &draft_id)
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraft(result)));
        }
        VintedDraftCommand::Validate { draft_id } => {
            let result = VintedDrafts::new(session, draft_api)
                .validate(portal, &draft_id)
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraftValidation(
                result,
            )));
        }
        _ => {}
    }
    let (operation, values) = match command {
        VintedDraftCommand::List { .. }
        | VintedDraftCommand::Show { .. }
        | VintedDraftCommand::Validate { .. } => unreachable!("read commands returned above"),
        VintedDraftCommand::Create(values) => (PublicationOperation::CreateDraft, Some(values)),
        VintedDraftCommand::Update { draft_id, values } => {
            (PublicationOperation::UpdateDraft { draft_id }, Some(values))
        }
        VintedDraftCommand::Publish { draft_id, values } => (
            PublicationOperation::CompleteDraft { draft_id },
            Some(PublicationInputArgs {
                input: values.input,
                image: values.image,
            }),
        ),
        VintedDraftCommand::Delete { draft_id } => {
            (PublicationOperation::DeleteDraft { draft_id }, None)
        }
    };
    execute_operation(
        portal,
        operation,
        values,
        session,
        publication_api,
        discovery_api,
        listing_api,
    )
    .await
}

async fn execute_operation(
    portal: PortalId,
    operation: PublicationOperation,
    values: Option<PublicationInputArgs>,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
    listing_api: &dyn VintedListingApi,
) -> Result<CommandOutcome, AppError> {
    let (mut input, images) = match values {
        Some(values) => {
            let value = read_input(&values.input)?;
            let input = resolve_publication_input(portal, value, session, discovery_api).await?;
            (Some(input), values.image)
        }
        None => (None, Vec::new()),
    };
    if let Some(input) = input.as_mut() {
        validate_listing_brand(portal, input, session, discovery_api).await?;
    }
    let publication = VintedPublication::new(api)
        .execute(operation.clone(), input, images)
        .await;
    let (result, pending) = match publication {
        Ok(result) => (result, false),
        Err(error) => {
            if !matches!(
                operation,
                PublicationOperation::Publish | PublicationOperation::CompleteDraft { .. }
            ) {
                return Err(error);
            }
            let Some(item_id) = review_pending_item_id(&error) else {
                return Err(error);
            };
            let inspection_action = publication_inspection_action(portal, item_id);
            let credentials = match session.credentials(portal).await {
                Ok(credentials) => credentials,
                Err(inspection_error) => {
                    return Err(publication_verification_uncertain(
                        error,
                        &inspection_action,
                        &inspection_error,
                    ));
                }
            };
            let (result, verification) = verify_confirmed_publication(
                portal,
                &operation,
                error,
                credentials,
                listing_api,
                discovery_api,
                PublicationVerificationConfig::default(),
            )
            .await?;
            (
                result,
                verification != PublicationVerificationOutcome::Public,
            )
        }
    };
    let next_actions = publication_next_actions(portal, result.item_id.as_deref());
    let mut outcome =
        CommandOutcome::new(CommandData::VintedPublication(result)).with_next_actions(next_actions);
    if pending {
        let timed_out = result_verification_status(&outcome.data)
            == Some(PublicationVerificationStatus::TimedOut);
        outcome = outcome.with_warnings(vec![if timed_out {
            Warning {
                code: "vinted.publication_verification_timed_out".to_owned(),
                message: "Vinted confirmed publication, but bounded account inspection did not expose the listing. Do not publish it again; run the inspection action to read its authoritative state.".to_owned(),
            }
        } else {
            Warning {
                code: "vinted.publication_review_pending".to_owned(),
                message: "Vinted confirmed publication and bounded inspection found the account listing pending review or hidden. Do not publish the item again; inspect the existing listing until review completes.".to_owned(),
            }
        }]);
    }
    Ok(outcome)
}

fn result_verification_status(data: &CommandData) -> Option<PublicationVerificationStatus> {
    let CommandData::VintedPublication(result) = data else {
        return None;
    };
    result.verification.as_ref().map(|value| value.status)
}

async fn verify_confirmed_publication(
    portal: PortalId,
    operation: &PublicationOperation,
    original_error: AppError,
    credentials: VintedCredentialRecord,
    listing_api: &dyn VintedListingApi,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
    config: PublicationVerificationConfig,
) -> Result<(PublicationResult, PublicationVerificationOutcome), AppError> {
    let item_id = review_pending_item_id(&original_error)
        .expect("confirmed publication verification requires an item ID")
        .to_owned();
    let inspection_action = publication_inspection_action(portal, &item_id);
    let inspection_session = move |_| Ok(credentials.clone());
    let listings =
        VintedListings::new(&inspection_session, listing_api).with_discovery(discovery_api);
    let started = Instant::now();
    let mut attempts = 0;
    let mut moderated = None;
    let mut missing = None;
    let mut last_error = None;

    while attempts < config.attempts.max(1) {
        let remaining = config.timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        attempts += 1;
        let inspection = tokio::time::timeout(
            remaining,
            listings.execute(
                portal,
                VintedListingRequest::Show {
                    item_id: item_id.clone(),
                },
            ),
        )
        .await;
        match inspection {
            Ok(Ok(VintedListingResult::Detail(detail))) => match detail.state {
                VintedListingState::Public => {
                    let result = confirmed_publication_result(
                        operation,
                        &original_error,
                        &detail,
                        PublicationVerificationStatus::Public,
                        attempts,
                    )
                    .expect("matching public listing produces a publication result");
                    return Ok((result, PublicationVerificationOutcome::Public));
                }
                VintedListingState::Moderated | VintedListingState::Hidden => {
                    moderated = Some(detail);
                }
                VintedListingState::Missing => missing = Some(detail),
                _ => {
                    let state = detail.state;
                    let inspection_error = AppError::upstream(
                        "vinted.publication_verification_unexpected_state",
                        format!("Vinted returned unexpected post-publication state {state:?}"),
                    );
                    return Err(publication_verification_uncertain(
                        original_error,
                        &inspection_action,
                        &inspection_error,
                    ));
                }
            },
            Ok(Ok(VintedListingResult::Collection(_))) => unreachable!("show returns detail"),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(AppError::upstream(
                    "vinted.publication_verification_request_timed_out",
                    "Vinted account inspection exceeded the verification deadline",
                ));
                break;
            }
        }
        if attempts < config.attempts.max(1) {
            let remaining = config.timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(config.interval.min(remaining)).await;
        }
    }

    if let Some(detail) = moderated {
        let result = confirmed_publication_result(
            operation,
            &original_error,
            &detail,
            PublicationVerificationStatus::Moderated,
            attempts,
        )
        .expect("matching moderated listing produces a publication result");
        return Ok((result, PublicationVerificationOutcome::Moderated));
    }
    if let Some(detail) = missing {
        let result = confirmed_publication_result(
            operation,
            &original_error,
            &detail,
            PublicationVerificationStatus::TimedOut,
            attempts,
        )
        .expect("matching missing listing produces a timed-out publication result");
        return Ok((result, PublicationVerificationOutcome::TimedOut));
    }
    let inspection_error = last_error.unwrap_or_else(|| {
        AppError::upstream(
            "vinted.publication_verification_unavailable",
            "Vinted account inspection returned no authoritative result",
        )
    });
    Err(publication_verification_uncertain(
        original_error,
        &inspection_action,
        &inspection_error,
    ))
}

fn publication_verification_uncertain(
    mut publication_error: AppError,
    inspection_action: &NextAction,
    inspection_error: &AppError,
) -> AppError {
    publication_error.code = "vinted.publication_verification_uncertain".to_owned();
    publication_error.message = "Vinted confirmed publication, but post-publication inspection failed before authoritative state became available".to_owned();
    publication_error.upstream_transient = inspection_error.upstream_transient;
    publication_error.safe_to_retry = false;
    publication_error.details = Some(Box::new(json!({
        "verification_status": "uncertain",
        "inspection_error": {
            "code": inspection_error.code,
            "upstream_transient": inspection_error.upstream_transient
        }
    })));
    publication_error.next_actions = Box::new(vec![NextAction {
        command: inspection_action.command.clone(),
    }]);
    publication_error
}

fn publication_inspection_action(portal: PortalId, item_id: &str) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} listing show {item_id}"),
    }
}

fn publication_next_actions(portal: PortalId, item_id: Option<&str>) -> Vec<NextAction> {
    item_id
        .map(|item_id| vec![publication_inspection_action(portal, item_id)])
        .unwrap_or_default()
}

async fn resolve_publication_input(
    portal: PortalId,
    value: serde_json::Value,
    session: &dyn VintedSearchSession,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
) -> Result<ListingInput, AppError> {
    if !has_semantic_values(&value) {
        return serde_json::from_value(value).map_err(invalid_listing_json);
    }
    let category_id = value
        .get("catalog_id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            AppError::validation(
                "vinted.catalog_id_required",
                "Semantic listing resolution requires an explicit runtime catalog_id",
            )
        })?;
    let composer = VintedPublicationComposer::new(session, discovery_api)
        .compose(portal, category_id, Some(value))
        .await?;
    let semantic_issues = composer
        .form
        .issues
        .iter()
        .filter(|issue| issue.code.starts_with("semantic_"))
        .collect::<Vec<_>>();
    if !semantic_issues.is_empty() {
        return Err(composer_correction_error(&composer, &semantic_issues));
    }
    serde_json::from_value(
        composer
            .normalized_input
            .clone()
            .expect("semantic composer retains normalized input"),
    )
    .map_err(invalid_listing_json)
}

fn composer_correction_error(
    composer: &VintedComposer,
    issues: &[&crate::domain::field::ValidationIssue],
) -> AppError {
    let fields = issues
        .iter()
        .map(|issue| issue.field.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let actions = composer
        .issue_actions
        .iter()
        .filter(|action| fields.contains(action.field.as_str()))
        .collect::<Vec<_>>();
    let mut error = AppError::validation(
        "vinted.listing_input_correction_required",
        "Semantic listing values require correction before publication",
    )
    .with_details(json!({
        "issues": issues,
        "correction_actions": actions,
        "semantic_resolutions": &composer.semantic_resolutions,
    }));
    error
        .next_actions
        .extend(actions.into_iter().map(|action| NextAction {
            command: action.command.clone(),
        }));
    error
}

fn invalid_listing_json(error: serde_json::Error) -> AppError {
    AppError::usage(format!(
        "Publication input must be valid Vinted listing JSON: {error}"
    ))
}

fn read_input(path: &PathBuf) -> Result<serde_json::Value, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                AppError::usage(format!("Failed to read publication input: {error}"))
            })?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read publication input `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("Publication input exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(invalid_listing_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::vinted::listing::ListingLookup;
    use serde_json::Value;
    use std::{
        collections::VecDeque,
        future::Future,
        pin::Pin,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct VerificationDiscoveryApi;

    impl VintedPublicationDiscoveryApi for VerificationDiscoveryApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _request: &'a crate::marketplace::vinted::publication_discovery::DiscoveryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            panic!("condition discovery should not be requested")
        }
    }

    struct VerificationApi {
        lookups: Mutex<VecDeque<Result<ListingLookup, AppError>>>,
    }

    impl VerificationApi {
        fn new(lookups: impl IntoIterator<Item = Result<ListingLookup, AppError>>) -> Self {
            Self {
                lookups: Mutex::new(lookups.into_iter().collect()),
            }
        }
    }

    impl VintedListingApi for VerificationApi {
        fn wardrobe_item<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _item_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>> {
            let result = self.lookups.lock().unwrap().pop_front().unwrap();
            Box::pin(async move { result })
        }

        fn item_for_edit<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            item_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            let result = Ok(json!({"data": {"item": {
                "id": item_id,
                "title": "Published item",
                "photos": []
            }}}));
            Box::pin(async move { result })
        }

        fn wardrobe_items<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _condition: &'a str,
            _page: usize,
            _per_page: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async {
                Ok(json!({"data": {
                    "items": [],
                    "pagination": {"total_pages": 1}
                }}))
            })
        }
    }

    fn verification_credentials() -> VintedCredentialRecord {
        VintedCredentialRecord::new_for_adapter(
            PortalId::Fi,
            "user".to_owned(),
            None,
            "access".to_owned(),
            "refresh".to_owned(),
            u64::MAX,
            "device".to_owned(),
            "anonymous".to_owned(),
            None,
        )
    }

    fn confirmed_inspection_error() -> AppError {
        let mut error = AppError::upstream("vinted.inspection_failed", "inspection failed");
        error.safe_to_retry = false;
        error.details = Some(Box::new(json!({"http_status": 404})));
        error.partial = Some(Box::new(json!({
            "mutation_status": "confirmed_applied",
            "item_id": "71",
            "uploaded_photos": [{"id": 9}],
            "uploaded_images": 1,
            "photo_action": "uploaded"
        })));
        error
    }

    fn verification_config(attempts: usize) -> PublicationVerificationConfig {
        PublicationVerificationConfig {
            timeout: Duration::from_secs(1),
            interval: Duration::ZERO,
            attempts,
        }
    }

    #[tokio::test]
    async fn verification_polls_moderated_state_until_listing_is_public() {
        let api = VerificationApi::new([
            Ok(ListingLookup::Found(json!({"data": {"item": {
                "id": 71,
                "status": "processing",
                "photos": []
            }}}))),
            Ok(ListingLookup::Found(json!({"data": {"item": {
                "id": 71,
                "status": "active",
                "url": "https://www.vinted.fi/items/71-published-item",
                "photos": []
            }}}))),
        ]);

        let (result, outcome) = verify_confirmed_publication(
            PortalId::Fi,
            &PublicationOperation::Publish,
            confirmed_inspection_error(),
            verification_credentials(),
            &api,
            &VerificationDiscoveryApi,
            verification_config(3),
        )
        .await
        .unwrap();

        assert_eq!(outcome, PublicationVerificationOutcome::Public);
        assert_eq!(
            result.status,
            crate::marketplace::vinted::publication::PublicationStatus::Succeeded
        );
        assert_eq!(result.authoritative_state, Some(VintedListingState::Public));
        assert_eq!(
            result.verification.unwrap().status,
            PublicationVerificationStatus::Public
        );
        assert!(!result.safe_to_retry);
        assert!(api.lookups.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bounded_moderation_inspection_preserves_authoritative_state() {
        let api = VerificationApi::new([Ok(ListingLookup::Found(json!({"data": {"item": {
            "id": 71,
            "status": "processing",
            "photos": []
        }}})))]);

        let (result, outcome) = verify_confirmed_publication(
            PortalId::Fi,
            &PublicationOperation::Publish,
            confirmed_inspection_error(),
            verification_credentials(),
            &api,
            &VerificationDiscoveryApi,
            verification_config(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome, PublicationVerificationOutcome::Moderated);
        assert_eq!(
            result.authoritative_state,
            Some(VintedListingState::Moderated)
        );
        assert_eq!(
            result.verification.unwrap().status,
            PublicationVerificationStatus::Moderated
        );
        assert!(!result.safe_to_retry);
    }

    #[tokio::test]
    async fn bounded_missing_inspection_returns_timed_out_result() {
        let api = VerificationApi::new([Ok(ListingLookup::Missing), Ok(ListingLookup::Missing)]);

        let (result, outcome) = verify_confirmed_publication(
            PortalId::Fi,
            &PublicationOperation::Publish,
            confirmed_inspection_error(),
            verification_credentials(),
            &api,
            &VerificationDiscoveryApi,
            verification_config(2),
        )
        .await
        .unwrap();

        assert_eq!(outcome, PublicationVerificationOutcome::TimedOut);
        assert_eq!(
            result.status,
            crate::marketplace::vinted::publication::PublicationStatus::Pending
        );
        assert_eq!(
            result.authoritative_state,
            Some(VintedListingState::Missing)
        );
        let verification = result.verification.unwrap();
        assert_eq!(verification.status, PublicationVerificationStatus::TimedOut);
        assert_eq!(verification.attempts, 2);
        assert!(!result.safe_to_retry);
        assert_eq!(
            publication_next_actions(PortalId::Fi, result.item_id.as_deref())[0].command,
            "flea vinted --portal fi listing show 71"
        );
    }

    #[tokio::test]
    async fn failed_inspection_is_uncertain_and_never_retryable() {
        let api = VerificationApi::new([Err(AppError::upstream(
            "fixture.inspection_failed",
            "inspection failed",
        ))]);

        let error = verify_confirmed_publication(
            PortalId::Fi,
            &PublicationOperation::Publish,
            confirmed_inspection_error(),
            verification_credentials(),
            &api,
            &VerificationDiscoveryApi,
            verification_config(1),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "vinted.publication_verification_uncertain");
        assert!(!error.safe_to_retry);
        assert_eq!(error.details.unwrap()["verification_status"], "uncertain");
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi listing show 71"
        );
    }

    #[test]
    fn publication_item_id_points_to_authoritative_inspection() {
        assert_eq!(
            publication_next_actions(PortalId::Fi, Some("9001"))[0].command,
            "flea vinted --portal fi listing show 9001"
        );
        assert!(publication_next_actions(PortalId::Fi, None).is_empty());
    }

    #[test]
    fn reads_complete_listing_input() {
        let path = std::env::temp_dir().join(format!(
            "flea-vinted-input-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            r#"{"title":"Item","description":"Description","catalog_id":1,"price":"5.00","currency":"EUR","package_size_id":2}"#,
        )
        .unwrap();
        let input = read_input(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(input["catalog_id"], 1);
        assert_eq!(input["currency"], "EUR");
    }
}
