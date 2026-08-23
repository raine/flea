use std::{future::Future, pin::Pin};

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::{
    error::{AppError, ExitClass},
    marketplace::vinted::{
        auth::{VintedAuthentication, VintedCredentialRecord},
        binding::VINTED_FI_BINDING,
    },
    transport::{RequestBody, Transport, TransportError, TransportErrorKind, TransportResponse},
};

const API_V2_PATH: &str = "/api/v2/";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    ConfirmedReady,
    ConfirmedBlocked,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SellingPrerequisiteType {
    PhoneVerification,
    EmailVerification,
    AddressOrPostalCode,
    PersonalInformation,
    TaxInformation,
    TwoFactorAuthentication,
    ListingRestriction,
    AccountVerification,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionReadiness {
    pub authenticated: bool,
    pub status: ReadinessState,
    pub health: &'static str,
    pub validation: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrerequisiteCheck {
    #[serde(rename = "type")]
    pub prerequisite_type: SellingPrerequisiteType,
    pub status: ReadinessState,
    pub source: &'static str,
    pub user_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicationReadiness {
    pub status: ReadinessState,
    pub session: SessionReadiness,
    pub prerequisites: Vec<PrerequisiteCheck>,
}

impl PublicationReadiness {
    pub fn blocked(&self) -> bool {
        self.status == ReadinessState::ConfirmedBlocked
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrerequisiteBlocker {
    pub prerequisite_type: SellingPrerequisiteType,
    pub user_action: String,
    pub action_url: Option<String>,
    pub response_code: Option<i64>,
    pub message_code: Option<String>,
}

pub trait VintedReadinessApi: Send + Sync {
    fn readiness<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
    ) -> Pin<Box<dyn Future<Output = Result<PublicationReadiness, AppError>> + Send + 'a>>;
}

pub struct HttpVintedReadinessApi {
    auth: VintedAuthentication,
    portal_api_base_url: String,
}

impl HttpVintedReadinessApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            portal_api_base_url: VINTED_FI_BINDING.portal_api_host.to_owned(),
        }
    }

    #[cfg(test)]
    fn with_portal_api_base_url(mut self, portal_api_base_url: String) -> Self {
        self.portal_api_base_url = portal_api_base_url;
        self
    }

    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        let mut url = Url::parse(&self.portal_api_base_url).map_err(|error| {
            AppError::unexpected("Vinted API binding is invalid").with_source(error)
        })?;
        url.set_path(&format!("{API_V2_PATH}{path}"));
        Ok(url)
    }

    async fn get(
        &self,
        credentials: &VintedCredentialRecord,
        path: &str,
    ) -> Result<(StatusCode, Value), AppError> {
        let url = self.endpoint(path)?;
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        debug_assert!(matches!(request.body, RequestBody::Empty));
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)?;
        decode_response(response)
    }

    async fn inspect(
        &self,
        credentials: &VintedCredentialRecord,
    ) -> Result<PublicationReadiness, AppError> {
        let (status, current_user) = self.get(credentials, "users/current").await?;
        if !status.is_success() {
            if let Some(blocker) = classify_prerequisite(&current_user) {
                return Ok(blocked_report(blocker, "current_user"));
            }
            return Err(readiness_http_error(status, &current_user));
        }
        let returned_id = current_user
            .pointer("/user/id")
            .and_then(value_as_id)
            .ok_or_else(invalid_response)?;
        if returned_id != credentials.user_id {
            return Err(invalid_response());
        }

        let mut checks = unknown_checks();
        apply_current_user(&mut checks, &current_user);

        let prompt_path = format!("users/{}/verifications/prompt", credentials.user_id);
        match self.get(credentials, &prompt_path).await {
            Ok((prompt_status, prompt)) if prompt_status.is_success() => {
                apply_prompt(&mut checks, &prompt);
            }
            Ok((_, prompt)) => {
                if let Some(blocker) = classify_prerequisite(&prompt) {
                    apply_blocker(&mut checks, blocker, "verification_prompt");
                }
            }
            Err(_) => {}
        }

        Ok(report(checks))
    }
}

impl Default for HttpVintedReadinessApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedReadinessApi for HttpVintedReadinessApi {
    fn readiness<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
    ) -> Pin<Box<dyn Future<Output = Result<PublicationReadiness, AppError>> + Send + 'a>> {
        Box::pin(self.inspect(credentials))
    }
}

fn report(checks: Vec<PrerequisiteCheck>) -> PublicationReadiness {
    let status = if checks
        .iter()
        .any(|check| check.status == ReadinessState::ConfirmedBlocked)
    {
        ReadinessState::ConfirmedBlocked
    } else if checks
        .iter()
        .all(|check| check.status == ReadinessState::ConfirmedReady)
    {
        ReadinessState::ConfirmedReady
    } else {
        ReadinessState::Unknown
    };
    PublicationReadiness {
        status,
        session: SessionReadiness {
            authenticated: true,
            status: ReadinessState::ConfirmedReady,
            health: "valid",
            validation: "online_current_user",
        },
        prerequisites: checks,
    }
}

fn blocked_report(blocker: PrerequisiteBlocker, source: &'static str) -> PublicationReadiness {
    let mut checks = unknown_checks();
    apply_blocker(&mut checks, blocker, source);
    report(checks)
}

fn unknown_checks() -> Vec<PrerequisiteCheck> {
    [
        SellingPrerequisiteType::PhoneVerification,
        SellingPrerequisiteType::EmailVerification,
        SellingPrerequisiteType::AddressOrPostalCode,
        SellingPrerequisiteType::PersonalInformation,
        SellingPrerequisiteType::TaxInformation,
        SellingPrerequisiteType::TwoFactorAuthentication,
        SellingPrerequisiteType::ListingRestriction,
        SellingPrerequisiteType::AccountVerification,
    ]
    .into_iter()
    .map(|prerequisite_type| PrerequisiteCheck {
        prerequisite_type,
        status: ReadinessState::Unknown,
        source: "not_exposed",
        user_action: unknown_action(prerequisite_type).to_owned(),
        action_url: None,
    })
    .collect()
}

fn unknown_action(kind: SellingPrerequisiteType) -> &'static str {
    match kind {
        SellingPrerequisiteType::PhoneVerification => {
            "Complete phone verification in Vinted if publication requests it."
        }
        SellingPrerequisiteType::EmailVerification => {
            "Complete email verification in Vinted if publication requests it."
        }
        SellingPrerequisiteType::AddressOrPostalCode => {
            "Complete the account address or postal code in Vinted if requested."
        }
        SellingPrerequisiteType::PersonalInformation => {
            "Review and confirm personal information in Vinted if requested."
        }
        SellingPrerequisiteType::TaxInformation => {
            "Complete required tax information in Vinted if requested."
        }
        SellingPrerequisiteType::TwoFactorAuthentication => {
            "Complete two-factor authentication in Vinted if requested."
        }
        SellingPrerequisiteType::ListingRestriction => {
            "Review any selling restriction in Vinted before continuing."
        }
        SellingPrerequisiteType::AccountVerification => {
            "Complete the account check in Vinted if publication requests it."
        }
    }
}

fn apply_current_user(checks: &mut [PrerequisiteCheck], value: &Value) {
    for (kind, path) in [
        (
            SellingPrerequisiteType::PhoneVerification,
            "/user/verification/phone/valid",
        ),
        (
            SellingPrerequisiteType::EmailVerification,
            "/user/verification/email/valid",
        ),
    ] {
        if value.pointer(path).and_then(Value::as_bool) == Some(true) {
            set_ready(checks, kind, "current_user");
        }
    }

    if has_nonempty_postal_code(value.pointer("/user/default_address")) {
        set_ready(
            checks,
            SellingPrerequisiteType::AddressOrPostalCode,
            "current_user",
        );
    }
    match value
        .pointer("/user/listing_restricted")
        .and_then(Value::as_bool)
    {
        Some(false) => set_ready(
            checks,
            SellingPrerequisiteType::ListingRestriction,
            "current_user",
        ),
        Some(true) => apply_blocker(
            checks,
            blocker(SellingPrerequisiteType::ListingRestriction, value),
            "current_user",
        ),
        None => {}
    }
    if value
        .pointer("/user/is_account_banned")
        .and_then(Value::as_bool)
        == Some(true)
    {
        apply_blocker(
            checks,
            blocker(SellingPrerequisiteType::ListingRestriction, value),
            "current_user",
        );
    }
    if value
        .pointer("/user/incomplete_tax_address")
        .and_then(Value::as_bool)
        == Some(true)
    {
        apply_blocker(
            checks,
            blocker(SellingPrerequisiteType::TaxInformation, value),
            "current_user",
        );
    }
}

fn has_nonempty_postal_code(value: Option<&Value>) -> bool {
    let Some(Value::Object(address)) = value else {
        return false;
    };
    ["postal_code", "postalCode", "zip_code", "zipCode"]
        .iter()
        .any(|key| {
            address
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
}

fn apply_prompt(checks: &mut [PrerequisiteCheck], value: &Value) {
    let Some(prompt) = value.get("prompt").filter(|prompt| !prompt.is_null()) else {
        return;
    };
    if prompt.get("mandatory").and_then(Value::as_bool) != Some(true) {
        return;
    }

    let mut applied = false;
    if let Some(methods) = prompt.get("methods").and_then(Value::as_array) {
        for method in methods.iter().filter_map(Value::as_str) {
            let kind = match normalized(method).as_str() {
                text if text.contains("phone") || text.contains("sms") => {
                    Some(SellingPrerequisiteType::PhoneVerification)
                }
                text if text.contains("email") => Some(SellingPrerequisiteType::EmailVerification),
                _ => None,
            };
            if let Some(kind) = kind {
                apply_blocker(checks, blocker(kind, prompt), "verification_prompt");
                applied = true;
            }
        }
    }
    if let Some(detected) = classify_prerequisite(prompt) {
        apply_blocker(checks, detected, "verification_prompt");
        applied = true;
    }
    if !applied {
        apply_blocker(
            checks,
            blocker(SellingPrerequisiteType::AccountVerification, prompt),
            "verification_prompt",
        );
    }
}

fn set_ready(
    checks: &mut [PrerequisiteCheck],
    kind: SellingPrerequisiteType,
    source: &'static str,
) {
    if let Some(check) = checks
        .iter_mut()
        .find(|check| check.prerequisite_type == kind)
    {
        check.status = ReadinessState::ConfirmedReady;
        check.source = source;
        check.user_action = "No user action is required by this check.".to_owned();
        check.action_url = None;
    }
}

fn apply_blocker(
    checks: &mut [PrerequisiteCheck],
    blocker: PrerequisiteBlocker,
    source: &'static str,
) {
    if let Some(check) = checks
        .iter_mut()
        .find(|check| check.prerequisite_type == blocker.prerequisite_type)
    {
        check.status = ReadinessState::ConfirmedBlocked;
        check.source = source;
        check.user_action = blocker.user_action;
        check.action_url = blocker.action_url;
    }
}

pub fn classify_prerequisite(value: &Value) -> Option<PrerequisiteBlocker> {
    let response_code = value.get("code").and_then(value_as_i64);
    let message_code = value
        .get("message_code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let text = normalized(&collect_signal_text(value));

    let kind = match response_code {
        Some(168) => Some(SellingPrerequisiteType::PhoneVerification),
        Some(167) => Some(SellingPrerequisiteType::EmailVerification),
        Some(108 | 115) => Some(SellingPrerequisiteType::PersonalInformation),
        Some(146 | 147 | 158 | 171 | 173) => Some(SellingPrerequisiteType::TwoFactorAuthentication),
        Some(152) => Some(SellingPrerequisiteType::TaxInformation),
        Some(162) => Some(SellingPrerequisiteType::ListingRestriction),
        _ if contains_any(
            &text,
            &[
                "phone_verification",
                "verify_phone",
                "phone verification",
                "sms_verification",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::PhoneVerification)
        }
        _ if contains_any(
            &text,
            &[
                "email_verification",
                "verify_email",
                "email verification",
                "email_confirmation",
                "confirm_email",
                "email confirmation",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::EmailVerification)
        }
        _ if contains_any(
            &text,
            &["global_two_factor", "second_factor", "two_factor", "2fa"],
        ) =>
        {
            Some(SellingPrerequisiteType::TwoFactorAuthentication)
        }
        _ if contains_any(
            &text,
            &[
                "incomplete_tax_address",
                "taxpayer_selling_blocked",
                "tax_information",
                "tax details",
                "tax_details",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::TaxInformation)
        }
        _ if contains_any(
            &text,
            &[
                "postal_code_required",
                "missing_postal_code",
                "zip_code_required",
                "address_required",
                "complete_address",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::AddressOrPostalCode)
        }
        _ if contains_any(
            &text,
            &[
                "personal_info",
                "identity_verification",
                "user_verification_required",
                "confirm_identity",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::PersonalInformation)
        }
        _ if contains_any(
            &text,
            &[
                "listing_restricted",
                "listing_restriction",
                "selling_restricted",
                "account_banned",
                "selling_blocked",
            ],
        ) =>
        {
            Some(SellingPrerequisiteType::ListingRestriction)
        }
        _ => None,
    }?;

    let mut result = blocker(kind, value);
    result.response_code = response_code;
    result.message_code = message_code;
    Some(result)
}

fn blocker(kind: SellingPrerequisiteType, value: &Value) -> PrerequisiteBlocker {
    PrerequisiteBlocker {
        prerequisite_type: kind,
        user_action: manual_action(kind).to_owned(),
        action_url: find_safe_url(value),
        response_code: None,
        message_code: None,
    }
}

fn manual_action(kind: SellingPrerequisiteType) -> &'static str {
    match kind {
        SellingPrerequisiteType::PhoneVerification => {
            "Open Vinted and complete phone verification. Flea does not send or enter SMS codes."
        }
        SellingPrerequisiteType::EmailVerification => {
            "Open Vinted and complete email verification. Flea does not send or enter email codes."
        }
        SellingPrerequisiteType::AddressOrPostalCode => {
            "Open Vinted and complete the account address or postal code."
        }
        SellingPrerequisiteType::PersonalInformation => {
            "Open Vinted and confirm the requested personal information or identity. Flea does not submit identity checks."
        }
        SellingPrerequisiteType::TaxInformation => {
            "Open Vinted and complete the requested tax information."
        }
        SellingPrerequisiteType::TwoFactorAuthentication => {
            "Open Vinted and complete two-factor authentication. Flea does not send or enter authentication codes."
        }
        SellingPrerequisiteType::ListingRestriction => {
            "Open Vinted and review the account or listing restriction before continuing."
        }
        SellingPrerequisiteType::AccountVerification => {
            "Open Vinted and complete the requested account verification. Flea does not automate verification."
        }
    }
}

fn collect_signal_text(value: &Value) -> String {
    fn collect(value: &Value, output: &mut String) {
        match value {
            Value::String(text) => {
                output.push(' ');
                output.push_str(text);
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    output.push(' ');
                    output.push_str(key);
                    collect(value, output);
                }
            }
            _ => {}
        }
    }
    let mut output = String::new();
    collect(value, &mut output);
    output
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn find_safe_url(value: &Value) -> Option<String> {
    for key in [
        "global_lock_fallback_url",
        "action_url",
        "redirect_url",
        "url",
    ] {
        if let Some(candidate) = find_string_key(value, key)
            && let Some(url) = safe_vinted_url(candidate)
        {
            return Some(url);
        }
    }
    None
}

fn find_string_key<'a>(value: &'a Value, wanted: &str) -> Option<&'a str> {
    match value {
        Value::Object(values) => {
            if let Some(found) = values.get(wanted).and_then(Value::as_str) {
                return Some(found);
            }
            values
                .values()
                .find_map(|value| find_string_key(value, wanted))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_key(value, wanted)),
        _ => None,
    }
}

fn safe_vinted_url(candidate: &str) -> Option<String> {
    let url = if candidate.starts_with('/') {
        VINTED_FI_BINDING
            .host
            .parse::<Url>()
            .ok()?
            .join(candidate)
            .ok()?
    } else {
        Url::parse(candidate).ok()?
    };
    let host = url.host_str()?;
    (url.scheme() == "https"
        && (host == "vinted.fi"
            || host.ends_with(".vinted.fi")
            || host == "vinted.com"
            || host.ends_with(".vinted.com")))
    .then(|| url.to_string())
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_as_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn decode_response(response: TransportResponse) -> Result<(StatusCode, Value), AppError> {
    let value = serde_json::from_slice(&response.body)
        .map_err(|error| invalid_response().with_source(error))?;
    Ok((response.status, value))
}

fn readiness_http_error(status: StatusCode, value: &Value) -> AppError {
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Vinted readiness inspection failed");
    let exit_class = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        ExitClass::Authentication
    } else {
        ExitClass::Upstream
    };
    AppError::new("vinted.readiness_failed", message, exit_class)
        .with_details(json!({ "http_status": status.as_u16() }))
}

fn invalid_response() -> AppError {
    AppError::upstream(
        "vinted.readiness_invalid_response",
        "Vinted returned an invalid publication-readiness response",
    )
}

fn transport_error(error: TransportError) -> AppError {
    let mut app_error = AppError::upstream(
        "vinted.readiness_transport_failed",
        "Vinted publication readiness could not be reached",
    )
    .with_source(error);
    app_error.upstream_transient = true;
    app_error.safe_to_retry = true;
    app_error
}

fn execution_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        invalid_response()
    } else {
        transport_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_known_prerequisite_class() {
        let cases = [
            (
                json!({"code": 168}),
                SellingPrerequisiteType::PhoneVerification,
            ),
            (
                json!({"code": "167"}),
                SellingPrerequisiteType::EmailVerification,
            ),
            (
                json!({"message_code": "postal_code_required"}),
                SellingPrerequisiteType::AddressOrPostalCode,
            ),
            (
                json!({"code": 115}),
                SellingPrerequisiteType::PersonalInformation,
            ),
            (
                json!({"code": 152}),
                SellingPrerequisiteType::TaxInformation,
            ),
            (
                json!({"code": 146}),
                SellingPrerequisiteType::TwoFactorAuthentication,
            ),
            (
                json!({"code": 162}),
                SellingPrerequisiteType::ListingRestriction,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(
                classify_prerequisite(&value).unwrap().prerequisite_type,
                expected
            );
        }
    }

    #[test]
    fn report_distinguishes_ready_blocked_and_unknown() {
        let unknown = unknown_checks();
        assert_eq!(report(unknown.clone()).status, ReadinessState::Unknown);

        let mut ready = unknown.clone();
        for check in &mut ready {
            check.status = ReadinessState::ConfirmedReady;
        }
        assert_eq!(report(ready).status, ReadinessState::ConfirmedReady);

        let mut blocked = unknown;
        blocked[0].status = ReadinessState::ConfirmedBlocked;
        assert_eq!(report(blocked).status, ReadinessState::ConfirmedBlocked);
    }

    #[test]
    fn current_user_confirms_only_exposed_ready_checks() {
        let mut checks = unknown_checks();
        apply_current_user(
            &mut checks,
            &json!({
                "user": {
                    "verification": {"phone": {"valid": true}, "email": {"valid": true}},
                    "default_address": {"postal_code": "00100"},
                    "listing_restricted": false
                }
            }),
        );
        for kind in [
            SellingPrerequisiteType::PhoneVerification,
            SellingPrerequisiteType::EmailVerification,
            SellingPrerequisiteType::AddressOrPostalCode,
            SellingPrerequisiteType::ListingRestriction,
        ] {
            assert_eq!(
                checks
                    .iter()
                    .find(|check| check.prerequisite_type == kind)
                    .unwrap()
                    .status,
                ReadinessState::ConfirmedReady
            );
        }
        assert_eq!(report(checks).status, ReadinessState::Unknown);
    }

    #[test]
    fn mandatory_prompt_blocks_and_keeps_only_safe_vinted_urls() {
        let mut checks = unknown_checks();
        apply_prompt(
            &mut checks,
            &json!({"prompt": {
                "mandatory": true,
                "methods": ["phone"],
                "action_url": "https://www.vinted.fi/member/verification"
            }}),
        );
        let phone = checks
            .iter()
            .find(|check| check.prerequisite_type == SellingPrerequisiteType::PhoneVerification)
            .unwrap();
        assert_eq!(phone.status, ReadinessState::ConfirmedBlocked);
        assert_eq!(
            phone.action_url.as_deref(),
            Some("https://www.vinted.fi/member/verification")
        );
        assert!(safe_vinted_url("https://evil.example/verify").is_none());
    }

    #[test]
    fn endpoint_can_be_rebound_for_fixture_servers() {
        let api =
            HttpVintedReadinessApi::new().with_portal_api_base_url("http://127.0.0.1:9".to_owned());
        assert_eq!(
            api.endpoint("users/current").unwrap().as_str(),
            "http://127.0.0.1:9/api/v2/users/current"
        );
    }
}
