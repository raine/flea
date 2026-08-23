#![allow(clippy::result_large_err)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    Method, StatusCode,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::{
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{PortalId, vinted::binding::VINTED_FI_BINDING},
    oauth::{SecretString, pkce_challenge, random_secret, random_uuid_secret, states_equal},
    storage::credentials::{CredentialStoreError, StoredCredential},
    transport::{
        RequestBody, ReqwestTransport, Transport, TransportError, TransportErrorKind,
        TransportErrorPhase, TransportRequest, TransportResponse,
    },
};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct VintedCredentialRecord {
    pub(crate) portal: PortalId,
    pub(crate) user_id: String,
    #[serde(default)]
    pub(crate) login: Option<String>,
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) access_expires_at_unix: u64,
    pub(crate) device_uuid: String,
    pub(crate) anonymous_id: String,
    #[serde(default)]
    pub(crate) user_device_token: Option<String>,
}

impl VintedCredentialRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new_for_adapter(
        portal: PortalId,
        user_id: String,
        login: Option<String>,
        access_token: String,
        refresh_token: String,
        access_expires_at_unix: u64,
        device_uuid: String,
        anonymous_id: String,
        user_device_token: Option<String>,
    ) -> Self {
        Self {
            portal,
            user_id,
            login,
            access_token,
            refresh_token,
            access_expires_at_unix,
            device_uuid,
            anonymous_id,
            user_device_token,
        }
    }
}

impl std::fmt::Debug for VintedCredentialRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VintedCredentialRecord")
            .field("portal", &self.portal)
            .field("user_id", &"[REDACTED]")
            .field("login", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("access_expires_at_unix", &self.access_expires_at_unix)
            .field("device_uuid", &"[REDACTED]")
            .field("anonymous_id", &"[REDACTED]")
            .field("user_device_token", &"[REDACTED]")
            .finish()
    }
}

impl StoredCredential for VintedCredentialRecord {
    fn account_id(&self) -> &str {
        &self.user_id
    }

    fn validate(&self) -> Result<(), CredentialStoreError> {
        let required = [
            self.user_id.as_str(),
            self.access_token.as_str(),
            self.refresh_token.as_str(),
            self.device_uuid.as_str(),
            self.anonymous_id.as_str(),
        ];
        if self.portal != PortalId::Fi
            || required.iter().any(|value| value.is_empty())
            || self.login.as_deref() == Some("")
            || self.user_device_token.as_deref() == Some("")
            || self.access_expires_at_unix == 0
        {
            return Err(CredentialStoreError::MissingRequiredValue);
        }
        Ok(())
    }
}

const PORTAL_BASE_URL: &str = VINTED_FI_BINDING.host;
const CLIENT_ID: &str = VINTED_FI_BINDING.client_id;
const SCOPE: &str = "user";
const CALLBACK_SCHEME: &str = VINTED_FI_BINDING.callback_scheme;
const REDIRECT_URI: &str = VINTED_FI_BINDING.redirect_uri;
const LOCALE: &str = VINTED_FI_BINDING.locale;
const ISO_LOCALE: &str = VINTED_FI_BINDING.iso_locale;
const APP_VERSION: &str = "26.32.0";
const USER_AGENT: &str = "vinted-android / fr.vinted v26.32.0 (263200 B; 15; Google; Pixel 6)";
const FLOW_LIFETIME: Duration = Duration::from_secs(10 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CALLBACK_URL_BYTES: usize = 8 * 1024;
const MAX_AUTHORIZATION_CODE_BYTES: usize = 4 * 1024;

pub struct VintedOAuthFlow {
    expires_at_unix: u64,
    state: SecretString,
    pkce_verifier: SecretString,
    device_uuid: SecretString,
    anonymous_id: SecretString,
}

impl std::fmt::Debug for VintedOAuthFlow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VintedOAuthFlow")
            .field("expires_at_unix", &self.expires_at_unix)
            .field("state", &self.state)
            .field("pkce_verifier", &self.pkce_verifier)
            .field("device_uuid", &self.device_uuid)
            .field("anonymous_id", &self.anonymous_id)
            .finish()
    }
}

pub struct VintedAuthStart {
    pub login_url: String,
    pub expires_at_unix: u64,
}

impl std::fmt::Debug for VintedAuthStart {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VintedAuthStart")
            .field("login_url", &"<redacted>")
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct VintedLoginResult {
    pub authenticated: bool,
    pub marketplace: &'static str,
    pub portal: &'static str,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    pub token_expires_in_seconds: u64,
    pub credential_storage: &'static str,
}

pub struct VintedAuthCompletion {
    pub credentials: VintedCredentialRecord,
    pub output: VintedLoginResult,
}

impl std::fmt::Debug for VintedAuthCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VintedAuthCompletion")
            .field("credentials", &self.credentials)
            .field("output", &self.output)
            .finish()
    }
}

#[derive(Clone)]
pub struct VintedAuthentication<T = ReqwestTransport> {
    transport: T,
    portal_base_url: String,
}

impl VintedAuthentication<ReqwestTransport> {
    pub fn new() -> Self {
        Self {
            transport: ReqwestTransport::default(),
            portal_base_url: PORTAL_BASE_URL.to_owned(),
        }
    }
}

impl<T: Transport> VintedAuthentication<T> {
    pub fn start(&self, now_unix: u64) -> Result<(VintedOAuthFlow, VintedAuthStart), AppError> {
        let verifier = generate_pkce_verifier();
        let challenge = pkce_challenge(verifier.expose());
        let state = random_secret(32);
        let expires_at_unix = now_unix
            .checked_add(FLOW_LIFETIME.as_secs())
            .ok_or_else(clock_error)?;
        let mut login_url = Url::parse(&format!("{}/oauth/authorize", self.portal_base_url))
            .expect("the configured Vinted authorization URL is valid");
        login_url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("prompt", "login"),
            ("state", state.expose()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("redirect_uri", REDIRECT_URI),
            ("vinted_in_app", "1"),
            ("locale", LOCALE),
        ]);
        let flow = VintedOAuthFlow {
            expires_at_unix,
            state,
            pkce_verifier: verifier,
            device_uuid: random_uuid_secret(),
            anonymous_id: random_uuid_secret(),
        };
        let start = VintedAuthStart {
            login_url: login_url.into(),
            expires_at_unix,
        };
        Ok((flow, start))
    }

    pub async fn complete(
        &self,
        flow: &VintedOAuthFlow,
        callback_url: &str,
        now_unix: u64,
    ) -> Result<VintedAuthCompletion, AppError> {
        if now_unix >= flow.expires_at_unix {
            return Err(restart_error(
                "vinted_auth.flow_expired",
                "the Vinted browser login flow expired",
            ));
        }
        let code = validate_callback(callback_url, flow.state.expose())?;
        let tokens = self.exchange_code(flow, code.expose()).await?;
        let account = self.current_user(flow, &tokens).await?;
        let expires_in = tokens
            .expires_in
            .filter(|expires_in| *expires_in > 0)
            .ok_or_else(|| unexpected_response("token_exchange"))?;
        let access_expires_at_unix = now_unix.checked_add(expires_in).ok_or_else(clock_error)?;
        let output = VintedLoginResult {
            authenticated: true,
            marketplace: "vinted",
            portal: "fi",
            user_id: account.id.clone(),
            login: account.login.clone(),
            token_expires_in_seconds: expires_in,
            credential_storage: "persisted",
        };
        let credentials = VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: account.id,
            login: account.login,
            access_token: tokens.access_token.expose().to_owned(),
            refresh_token: tokens.refresh_token.expose().to_owned(),
            access_expires_at_unix,
            device_uuid: flow.device_uuid.expose().to_owned(),
            anonymous_id: flow.anonymous_id.expose().to_owned(),
            user_device_token: tokens
                .user_device_token
                .as_ref()
                .map(|token| token.expose().to_owned()),
        };
        Ok(VintedAuthCompletion {
            credentials,
            output,
        })
    }

    async fn exchange_code(
        &self,
        flow: &VintedOAuthFlow,
        code: &str,
    ) -> Result<VintedTokens, AppError> {
        let mut headers = native_headers(
            flow.device_uuid.expose(),
            flow.anonymous_id.expose(),
            token_execution_error,
        )?;
        insert_header(&mut headers, "x-v-udt", "").map_err(token_execution_error)?;
        let request = transport_request(
            Method::POST,
            format!("{}/oauth/token", self.portal_base_url),
            headers,
            form_body(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("scope", SCOPE),
                ("code", code),
                ("redirect_uri", REDIRECT_URI),
                ("code_verifier", flow.pkce_verifier.expose()),
            ]),
        );
        let response = self
            .transport
            .execute(request)
            .await
            .map_err(token_execution_error)?;
        ensure_token_success(response.status)?;
        let user_device_token = response
            .headers
            .get("x-v-udt")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(|value| SecretString::new(value.to_owned()));
        let decoded: TokenResponse = decode_json(response, "token_exchange")?;
        if decoded.access_token.is_empty()
            || decoded.refresh_token.is_empty()
            || !decoded.token_type.eq_ignore_ascii_case("bearer")
        {
            return Err(unexpected_response("token_exchange"));
        }
        Ok(VintedTokens {
            access_token: SecretString::new(decoded.access_token),
            refresh_token: SecretString::new(decoded.refresh_token),
            token_type: decoded.token_type,
            expires_in: decoded.expires_in,
            user_device_token,
        })
    }

    async fn current_user(
        &self,
        flow: &VintedOAuthFlow,
        tokens: &VintedTokens,
    ) -> Result<VintedAccount, AppError> {
        self.request_current_user(
            flow.device_uuid.expose(),
            flow.anonymous_id.expose(),
            tokens.access_token.expose(),
            tokens.user_device_token.as_ref().map(SecretString::expose),
        )
        .await
    }

    pub(crate) fn authenticated_request(
        &self,
        method: Method,
        url: String,
        credentials: &VintedCredentialRecord,
        max_response_bytes: usize,
        invalid_header: fn(TransportError) -> AppError,
    ) -> Result<TransportRequest, AppError> {
        let mut headers = native_headers(
            &credentials.device_uuid,
            &credentials.anonymous_id,
            invalid_header,
        )?;
        insert_header(
            &mut headers,
            "authorization",
            &format!("Bearer {}", credentials.access_token),
        )
        .map_err(invalid_header)?;
        insert_header(
            &mut headers,
            "x-v-udt",
            credentials.user_device_token.as_deref().unwrap_or(""),
        )
        .map_err(invalid_header)?;
        let jwt = jwt_request_context(&credentials.access_token);
        if let Some(user_id) = jwt.user_id {
            insert_header(&mut headers, "x-v-uid", user_id.expose()).map_err(invalid_header)?;
        }
        if let Some(session_id) = jwt.session_id {
            insert_header(&mut headers, "x-v-sid", session_id.expose()).map_err(invalid_header)?;
        }
        Ok(TransportRequest {
            method,
            url,
            headers,
            body: RequestBody::Empty,
            deadline: REQUEST_TIMEOUT,
            max_response_bytes,
        })
    }

    pub(crate) fn executor(&self) -> &T {
        &self.transport
    }

    pub async fn validate_credentials(
        &self,
        credentials: &VintedCredentialRecord,
    ) -> Result<(String, Option<String>), AppError> {
        let account = self
            .request_current_user(
                &credentials.device_uuid,
                &credentials.anonymous_id,
                &credentials.access_token,
                credentials.user_device_token.as_deref(),
            )
            .await?;
        if account.id != credentials.user_id {
            return Err(unexpected_response("current_user"));
        }
        Ok((account.id, account.login))
    }

    async fn request_current_user(
        &self,
        device_uuid: &str,
        anonymous_id: &str,
        access_token: &str,
        user_device_token: Option<&str>,
    ) -> Result<VintedAccount, AppError> {
        let mut headers = native_headers(device_uuid, anonymous_id, validation_execution_error)?;
        insert_header(
            &mut headers,
            "authorization",
            &format!("Bearer {access_token}"),
        )
        .map_err(validation_execution_error)?;
        insert_header(&mut headers, "x-v-udt", user_device_token.unwrap_or(""))
            .map_err(validation_execution_error)?;
        let jwt = jwt_request_context(access_token);
        if let Some(user_id) = jwt.user_id {
            insert_header(&mut headers, "x-v-uid", user_id.expose())
                .map_err(validation_execution_error)?;
        }
        if let Some(session_id) = jwt.session_id {
            insert_header(&mut headers, "x-v-sid", session_id.expose())
                .map_err(validation_execution_error)?;
        }
        let request = transport_request(
            Method::GET,
            format!("{}/api/v2/users/current", self.portal_base_url),
            headers,
            RequestBody::Empty,
        );
        let response = self
            .transport
            .execute(request)
            .await
            .map_err(validation_execution_error)?;
        ensure_validation_success(response.status)?;
        let decoded: CurrentUserResponse = decode_json(response, "current_user")?;
        let id = identifier(decoded.user.id).ok_or_else(|| unexpected_response("current_user"))?;
        if decoded.user.login.as_deref() == Some("") {
            return Err(unexpected_response("current_user"));
        }
        Ok(VintedAccount {
            id,
            login: decoded.user.login,
        })
    }

    #[cfg(test)]
    fn with_portal_base_url(mut self, portal_base_url: String) -> Self {
        self.portal_base_url = portal_base_url;
        self
    }
}

impl Default for VintedAuthentication<ReqwestTransport> {
    fn default() -> Self {
        Self::new()
    }
}

struct VintedTokens {
    access_token: SecretString,
    refresh_token: SecretString,
    token_type: String,
    expires_in: Option<u64>,
    user_device_token: Option<SecretString>,
}

impl std::fmt::Debug for VintedTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VintedTokens")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("user_device_token", &self.user_device_token)
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct CurrentUserResponse {
    user: CurrentUser,
}

#[derive(Deserialize)]
struct CurrentUser {
    id: serde_json::Value,
    #[serde(default)]
    login: Option<String>,
}

struct VintedAccount {
    id: String,
    login: Option<String>,
}

#[derive(Default)]
struct JwtRequestContext {
    user_id: Option<SecretString>,
    session_id: Option<SecretString>,
}

fn generate_pkce_verifier() -> SecretString {
    random_secret(48)
}

fn validate_callback(callback_url: &str, expected_state: &str) -> Result<SecretString, AppError> {
    if callback_url.len() > MAX_CALLBACK_URL_BYTES {
        return Err(invalid_callback());
    }
    let parsed = Url::parse(callback_url).map_err(|_| invalid_callback())?;
    if parsed.scheme() != CALLBACK_SCHEME
        || parsed.host_str() != Some("auth")
        || !matches!(parsed.path(), "" | "/")
        || parsed.port().is_some()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid_callback());
    }

    let mut code = None;
    let mut state = None;
    let mut oauth_error = false;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if state.is_none() => state = Some(value.into_owned()),
            "error" if !oauth_error => oauth_error = true,
            "code" | "state" | "error" => return Err(invalid_callback()),
            _ => return Err(invalid_callback()),
        }
    }
    if !state.as_deref().is_some_and(|actual| {
        states_equal(actual, expected_state, b"vinted-oauth-state-comparison")
    }) {
        return Err(restart_error(
            "vinted_auth.state_mismatch",
            "the Vinted callback state is invalid",
        ));
    }
    if oauth_error {
        return Err(restart_error(
            "vinted_auth.authorization_denied",
            "Vinted did not authorize the browser login",
        ));
    }
    let code = code
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_AUTHORIZATION_CODE_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(invalid_callback)?;
    Ok(SecretString::new(code))
}

fn jwt_request_context(access_token: &str) -> JwtRequestContext {
    #[derive(Deserialize)]
    struct Claims {
        #[serde(default)]
        sub: Option<serde_json::Value>,
        #[serde(default)]
        sid: Option<serde_json::Value>,
    }

    let Some(payload) = access_token.split('.').nth(1) else {
        return JwtRequestContext::default();
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return JwtRequestContext::default();
    };
    let Ok(claims) = serde_json::from_slice::<Claims>(&bytes) else {
        return JwtRequestContext::default();
    };
    JwtRequestContext {
        user_id: claims.sub.and_then(identifier).map(SecretString::new),
        session_id: claims.sid.and_then(identifier).map(SecretString::new),
    }
}

fn identifier(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) if !value.is_empty() => Some(value),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn native_headers(
    device_uuid: &str,
    anonymous_id: &str,
    invalid_header: fn(TransportError) -> AppError,
) -> Result<HeaderMap, AppError> {
    let local_time = unix_time_millis()?.to_string();
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("accept", "application/json"),
        ("user-agent", USER_AGENT),
        ("x-platform", "android"),
        ("x-portal", VINTED_FI_BINDING.portal_header),
        ("x-app-version", APP_VERSION),
        ("x-os-version", "15"),
        ("x-device-model", "Google Pixel 6"),
        ("x-screen-width", "1080"),
        ("x-screen-height", "2400"),
        ("x-local-time", &local_time),
        ("x-anon-id", anonymous_id),
        ("x-device-uuid", device_uuid),
        ("locale", ISO_LOCALE),
        ("accept-language", LOCALE),
    ] {
        insert_header(&mut headers, name, value).map_err(invalid_header)?;
    }
    Ok(headers)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), TransportError> {
    let value = HeaderValue::from_str(value)
        .map_err(|_| TransportError::request(TransportErrorKind::Other))?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

fn form_body(values: &[(&str, &str)]) -> RequestBody {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(values.iter().copied());
    RequestBody::Bytes(serializer.finish().into_bytes())
}

fn transport_request(
    method: Method,
    url: String,
    mut headers: HeaderMap,
    body: RequestBody,
) -> TransportRequest {
    if matches!(body, RequestBody::Bytes(_)) {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
    }
    TransportRequest {
        method,
        url,
        headers,
        body,
        deadline: REQUEST_TIMEOUT,
        max_response_bytes: MAX_RESPONSE_BYTES,
    }
}

fn token_execution_error(error: TransportError) -> AppError {
    if error.phase == TransportErrorPhase::Response {
        unexpected_response("token_exchange").with_source(error)
    } else {
        token_transport_error().with_source(error)
    }
}

fn validation_execution_error(error: TransportError) -> AppError {
    if error.phase == TransportErrorPhase::Response {
        unexpected_response("current_user").with_source(error)
    } else {
        validation_transport_error().with_source(error)
    }
}

fn decode_json<T: DeserializeOwned>(
    response: TransportResponse,
    stage: &'static str,
) -> Result<T, AppError> {
    serde_json::from_slice(&response.body).map_err(|_| unexpected_response(stage))
}

fn ensure_token_success(status: StatusCode) -> Result<(), AppError> {
    if status.is_success() {
        return Ok(());
    }
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(restart_error(
            "vinted_auth.token_exchange_unavailable",
            "the Vinted token exchange returned a temporary failure with an ambiguous authorization-code outcome",
        )
        .with_details(serde_json::json!({
            "stage": "token_exchange",
            "status": status.as_u16()
        }))
        .retry_classification(crate::retry::RetryClassification {
            upstream_transient: true,
            safe_to_retry: false,
        }));
    }
    Err(restart_error(
        "vinted_auth.exchange_rejected",
        "Vinted rejected the authorization-code exchange",
    )
    .with_details(serde_json::json!({
        "stage": "token_exchange",
        "status": status.as_u16()
    })))
}

fn ensure_validation_success(status: StatusCode) -> Result<(), AppError> {
    if status.is_success() {
        return Ok(());
    }
    let mut error = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        AppError::authentication(
            "vinted_auth.validation_rejected",
            "Vinted rejected the issued access token during account validation",
        )
    } else {
        AppError::upstream(
            "vinted_auth.validation_failed",
            "the Vinted current-user validation request failed",
        )
    };
    error.details = Some(Box::new(serde_json::json!({
        "stage": "current_user",
        "status": status.as_u16()
    })));
    error.next_actions.push(restart_action());
    Err(error)
}

fn token_transport_error() -> AppError {
    restart_error(
        "vinted_auth.token_exchange_failed",
        "the Vinted token exchange could not be completed; restart browser login because the authorization-code outcome is ambiguous",
    )
    .with_details(serde_json::json!({ "stage": "token_exchange" }))
}

fn validation_transport_error() -> AppError {
    restart_error(
        "vinted_auth.validation_transport_failed",
        "the issued Vinted access token could not be validated",
    )
    .with_details(serde_json::json!({ "stage": "current_user" }))
}

fn unexpected_response(stage: &'static str) -> AppError {
    restart_error(
        "vinted_auth.unexpected_response",
        "Vinted returned an unexpected authentication response",
    )
    .with_details(serde_json::json!({ "stage": stage }))
}

fn invalid_callback() -> AppError {
    restart_error(
        "vinted_auth.callback_invalid",
        "the Vinted authentication callback is invalid",
    )
}

fn restart_error(code: &str, message: &str) -> AppError {
    let mut error = AppError::new(code, message, ExitClass::Authentication);
    error.next_actions.push(restart_action());
    error
}

fn restart_action() -> NextAction {
    NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    }
}

fn clock_error() -> AppError {
    restart_error(
        "vinted_auth.clock_invalid",
        "the system clock cannot represent the Vinted login flow expiry",
    )
}

fn unix_time_millis() -> Result<u128, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|_| clock_error())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    use super::*;

    type MockResponse = (&'static str, String, Vec<(&'static str, &'static str)>);

    fn mock_service(
        responses: Vec<MockResponse>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            for (status, body, headers) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let expected_length = loop {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&request[..headers_end]);
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>())
                        })
                        .transpose()
                        .unwrap()
                        .unwrap_or(0);
                    if request.len() >= headers_end + 4 + content_length {
                        break headers_end + 4 + content_length;
                    }
                };
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request[..expected_length].to_vec()).unwrap());
                let extra_headers = headers
                    .into_iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (base_url, requests, worker)
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn start_builds_the_provider_selector_request() {
        let auth = VintedAuthentication::new();
        let (first, start) = auth.start(1_000).unwrap();
        let (second, _) = auth.start(1_000).unwrap();
        let url = Url::parse(&start.login_url).unwrap();
        let query: HashMap<_, _> = url.query_pairs().collect();

        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://www.vinted.fi/oauth/authorize"
        );
        assert_eq!(query.get("response_type").unwrap(), "code");
        assert_eq!(query.get("client_id").unwrap(), "android");
        assert_eq!(query.get("scope").unwrap(), "user");
        assert_eq!(query.get("prompt").unwrap(), "login");
        assert_eq!(query.get("redirect_uri").unwrap(), REDIRECT_URI);
        assert_eq!(query.get("vinted_in_app").unwrap(), "1");
        assert_eq!(query.get("locale").unwrap(), "fi");
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(start.expires_at_unix, 1_600);
        assert_ne!(first.state.expose(), second.state.expose());
        assert_ne!(first.pkce_verifier.expose(), second.pkce_verifier.expose());
        assert_ne!(first.device_uuid.expose(), second.device_uuid.expose());
        let debug = format!("{first:?} {start:?}");
        for secret in [
            first.state.expose(),
            first.pkce_verifier.expose(),
            first.device_uuid.expose(),
            first.anonymous_id.expose(),
            &start.login_url,
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn callback_requires_the_exact_target_state_and_query() {
        assert_eq!(
            validate_callback("vintedfr://auth?code=ok&state=expected", "expected")
                .unwrap()
                .expose(),
            "ok"
        );
        for callback in [
            "vintedfi://auth?code=ok&state=expected",
            "vintedfr://other?code=ok&state=expected",
            "vintedfr://auth/path?code=ok&state=expected",
            "vintedfr://auth?code=one&code=two&state=expected",
            "vintedfr://auth?code=ok&state=expected&extra=value",
            "vintedfr://auth?code=ok&state=wrong",
        ] {
            assert!(
                validate_callback(callback, "expected").is_err(),
                "{callback}"
            );
        }
    }

    #[tokio::test]
    async fn exchanges_the_code_and_validates_the_account_with_native_context() {
        let claims = URL_SAFE_NO_PAD.encode(br#"{"sub":"user-42","sid":"session-7"}"#);
        let access_token = format!("header.{claims}.signature");
        let token_body = serde_json::json!({
            "access_token": access_token,
            "refresh_token": "refresh-secret",
            "token_type": "Bearer",
            "expires_in": 3600
        })
        .to_string();
        let current_body = serde_json::json!({
            "code": 0,
            "user": { "id": 42, "login": "fixture-user" }
        })
        .to_string();
        let (base_url, requests, worker) = mock_service(vec![
            (
                "200 OK",
                token_body,
                vec![("X-V-Udt", "device-token-secret")],
            ),
            ("200 OK", current_body, Vec::new()),
        ]);
        let auth = VintedAuthentication::new().with_portal_base_url(base_url);
        let (flow, _) = auth.start(1_000).unwrap();
        let callback = format!(
            "vintedfr://auth?code=authorization-code&state={}",
            flow.state.expose()
        );

        let result = auth.complete(&flow, &callback, 1_001).await.unwrap();
        worker.join().unwrap();

        assert_eq!(result.output.user_id, "42");
        assert_eq!(result.output.login.as_deref(), Some("fixture-user"));
        assert_eq!(result.output.token_expires_in_seconds, 3600);
        assert_eq!(result.output.credential_storage, "persisted");
        assert_eq!(result.credentials.user_id, "42");
        assert_eq!(result.credentials.access_expires_at_unix, 4_601);
        assert_eq!(result.credentials.refresh_token, "refresh-secret");
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with("POST /oauth/token HTTP/1.1"));
        assert!(requests[0].contains("grant_type=authorization_code"));
        assert!(requests[0].contains("redirect_uri=vintedfr%3A%2F%2Fauth"));
        assert!(requests[0].contains("x-platform: android"));
        assert!(requests[0].contains("x-portal: fr"));
        assert!(requests[0].contains("locale: fi-FI"));
        assert!(requests[1].starts_with("GET /api/v2/users/current HTTP/1.1"));
        assert!(requests[1].contains("authorization: Bearer header."));
        assert!(requests[1].contains("x-v-uid: user-42"));
        assert!(requests[1].contains("x-v-sid: session-7"));
        assert!(requests[1].contains("x-v-udt: device-token-secret"));
    }

    #[test]
    fn token_debug_output_redacts_all_secret_material() {
        let tokens = VintedTokens {
            access_token: SecretString::new("access-secret".into()),
            refresh_token: SecretString::new("refresh-secret".into()),
            token_type: "Bearer".into(),
            expires_in: Some(3_600),
            user_device_token: Some(SecretString::new("device-token-secret".into())),
        };
        let debug = format!("{tokens:?}");

        for secret in ["access-secret", "refresh-secret", "device-token-secret"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn ambiguous_code_exchange_failure_requires_a_fresh_login() {
        let error = ensure_token_success(StatusCode::BAD_GATEWAY).unwrap_err();

        assert!(error.upstream_transient);
        assert!(!error.safe_to_retry);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );
    }
}
