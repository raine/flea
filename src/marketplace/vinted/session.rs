use serde::Serialize;
use serde_json::Value;

use crate::{
    cli::outcome::CommandOutcome,
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        CapabilityMaturity, MarketplaceContext, PortalId,
        vinted::auth::{VintedAuthentication, VintedCredentialRecord},
    },
    storage::{StatePaths, credentials::TypedCredentialStore},
};

type VintedCredentialStore = TypedCredentialStore<VintedCredentialRecord>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthOperation {
    Login,
    Status,
    Logout,
}

pub(crate) async fn execute_auth(
    portal: PortalId,
    operation: AuthOperation,
) -> Result<CommandOutcome, AppError> {
    if portal != PortalId::Fi {
        return Err(AppError::usage("the selected Vinted portal is unavailable"));
    }
    let paths = StatePaths::discover(MarketplaceContext::VINTED_FI)
        .map_err(|error| storage_error(error, "discover"))?;
    match operation {
        AuthOperation::Login => execute_login(paths).await.map(Into::into),
        AuthOperation::Status => execute_status(paths).await,
        AuthOperation::Logout => execute_logout(paths).map(Into::into),
    }
}

async fn execute_login(paths: StatePaths) -> Result<Value, AppError> {
    let auth = VintedAuthentication::new();
    let (flow, start) = auth.start(unix_time_now()?)?;
    let callback = super::interactive::open_and_capture_callback(
        &paths,
        &start.login_url,
        start.expires_at_unix,
    )?;
    let completion = auth.complete(&flow, &callback, unix_time_now()?).await?;
    VintedCredentialStore::new(paths)
        .save(&completion.credentials)
        .map_err(|error| storage_error(error, "write"))?;
    serialize(completion.output, "Vinted login")
}

#[derive(Serialize)]
struct VintedAuthStatus {
    authenticated: bool,
    health: &'static str,
    validation: &'static str,
    refresh_maturity: CapabilityMaturity,
    refresh_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_expires_at_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_seconds: Option<u64>,
    #[serde(skip)]
    next_actions: Vec<NextAction>,
}

async fn execute_status(paths: StatePaths) -> Result<CommandOutcome, AppError> {
    let now = unix_time_now()?;
    let Some(credentials) = VintedCredentialStore::new(paths)
        .load()
        .map_err(|error| storage_error(error, "read"))?
    else {
        return serialize_status(unavailable_status("missing", "local_storage"));
    };
    if credentials.access_expires_at_unix <= now {
        return serialize_status(VintedAuthStatus {
            access_expires_at_unix: Some(credentials.access_expires_at_unix),
            ..unavailable_status("expired", "local_expiry")
        });
    }

    let (_, login) = match VintedAuthentication::new()
        .validate_credentials(&credentials)
        .await
    {
        Ok(account) => account,
        Err(error) => {
            return serialize_status(validation_failure_status(&credentials, &error));
        }
    };
    serialize_status(VintedAuthStatus {
        authenticated: true,
        health: "valid",
        validation: "online_current_user",
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: Some(credentials.user_id),
        login,
        access_expires_at_unix: Some(credentials.access_expires_at_unix),
        expires_in_seconds: Some(credentials.access_expires_at_unix.saturating_sub(now)),
        next_actions: Vec::new(),
    })
}

fn execute_logout(paths: StatePaths) -> Result<Value, AppError> {
    VintedCredentialStore::new(paths)
        .delete()
        .map_err(|error| storage_error(error, "delete"))?;
    Ok(serde_json::json!({
        "authenticated": false,
        "marketplace": "vinted",
        "portal": "fi",
    }))
}

fn unavailable_status(health: &'static str, validation: &'static str) -> VintedAuthStatus {
    VintedAuthStatus {
        authenticated: false,
        health,
        validation,
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: None,
        login: None,
        access_expires_at_unix: None,
        expires_in_seconds: None,
        next_actions: vec![retry_action()],
    }
}

fn validation_failure_status(
    credentials: &VintedCredentialRecord,
    error: &AppError,
) -> VintedAuthStatus {
    let rejected = error.code == "vinted_auth.validation_rejected";
    VintedAuthStatus {
        authenticated: false,
        health: if rejected {
            "rejected"
        } else {
            "temporarily_unavailable"
        },
        validation: "online_current_user",
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: Some(credentials.user_id.clone()),
        login: credentials.login.clone(),
        access_expires_at_unix: Some(credentials.access_expires_at_unix),
        expires_in_seconds: None,
        next_actions: vec![if rejected {
            retry_action()
        } else {
            NextAction {
                command: crate::invocation::vinted_fi("auth status"),
            }
        }],
    }
}

fn serialize_status(mut status: VintedAuthStatus) -> Result<CommandOutcome, AppError> {
    let next_actions = std::mem::take(&mut status.next_actions);
    serialize(status, "Vinted auth status")
        .map(|data| CommandOutcome::new(data).with_next_actions(next_actions))
}

fn serialize(value: impl Serialize, stage: &'static str) -> Result<Value, AppError> {
    serde_json::to_value(value).map_err(|error| {
        AppError::output(format!("failed to serialize {stage} output")).with_source(error)
    })
}

fn storage_error(
    error: impl std::error::Error + Send + Sync + 'static,
    operation: &'static str,
) -> AppError {
    let mut result = AppError::new(
        "auth.storage_failed",
        "Vinted authentication state could not be updated safely",
        ExitClass::Authentication,
    )
    .with_details(serde_json::json!({ "operation": operation }))
    .with_source(error);
    result.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth status"),
    });
    result
}

pub(crate) fn credentials(portal: PortalId) -> Result<VintedCredentialRecord, AppError> {
    if portal != PortalId::Fi {
        return Err(AppError::usage("the selected Vinted portal is unavailable"));
    }
    let paths = StatePaths::discover(MarketplaceContext::VINTED_FI)
        .map_err(|error| vinted_auth_storage(error, "discover"))?;
    let credentials = VintedCredentialStore::new(paths)
        .load()
        .map_err(|error| vinted_auth_storage(error, "read"))?
        .ok_or_else(vinted_auth_required)?;
    if credentials.access_expires_at_unix <= unix_time_now()? {
        return Err(vinted_auth_required());
    }
    Ok(credentials)
}

fn vinted_auth_storage(
    error: impl std::error::Error + Send + Sync + 'static,
    operation: &'static str,
) -> AppError {
    let mut result = AppError::new(
        "vinted_auth.storage_failed",
        "Vinted authentication credential storage is unavailable",
        ExitClass::Authentication,
    )
    .with_details(serde_json::json!({ "operation": operation }))
    .with_source(error);
    result.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth status"),
    });
    result
}

fn vinted_auth_required() -> AppError {
    let mut error =
        AppError::authentication("vinted_auth.required", "Vinted authentication is required");
    error.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    });
    error
}

fn retry_action() -> NextAction {
    NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    }
}

fn unix_time_now() -> Result<u64, AppError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            AppError::new(
                "auth.clock_invalid",
                "the system clock is invalid",
                ExitClass::Authentication,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: "user-1".to_owned(),
            login: Some("fixture".to_owned()),
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            access_expires_at_unix: 2_000,
            device_uuid: "device".to_owned(),
            anonymous_id: "anonymous".to_owned(),
            user_device_token: None,
        }
    }

    #[tokio::test]
    async fn missing_status_preserves_the_exact_auth_document() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            MarketplaceContext::VINTED_FI,
        );

        let status = execute_status(paths).await.unwrap();

        assert_eq!(
            status,
            serde_json::json!({
                "authenticated": false,
                "health": "missing",
                "validation": "local_storage",
                "refresh_maturity": "source_derived",
                "refresh_performed": false,
            })
        );
        assert_eq!(
            status.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );
    }

    #[test]
    fn status_maps_online_validation_failures_to_health_documents() {
        let rejected =
            AppError::authentication("vinted_auth.validation_rejected", "token rejected");
        let rejected = validation_failure_status(&credentials(), &rejected);
        assert_eq!(rejected.health, "rejected");
        assert_eq!(
            rejected.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );

        let unavailable = AppError::upstream(
            "vinted_auth.validation_transport_failed",
            "network unavailable",
        );
        let unavailable = validation_failure_status(&credentials(), &unavailable);
        assert_eq!(unavailable.health, "temporarily_unavailable");
        assert_eq!(
            unavailable.next_actions[0].command,
            "flea vinted --portal fi auth status"
        );
    }
}
