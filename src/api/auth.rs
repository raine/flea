#![allow(clippy::result_large_err)]

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256, Sha512};
use url::Url;
use uuid::Uuid;

use crate::{
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
};

pub const CLIENT_ID: &str = "6079834b9b0b741812e7e91f";
pub const EXCHANGE_CLIENT_ID: &str = "650421cf50eeae31ecd2a2d3";
pub const CALLBACK_SCHEME: &str = "fi.tori.www.6079834b9b0b741812e7e91f";
pub const REDIRECT_URI: &str = "fi.tori.www.6079834b9b0b741812e7e91f://login";
pub const DEFAULT_FLOW_LIFETIME: Duration = Duration::from_secs(10 * 60);

const LOGIN_BASE_URL: &str = "https://login.vend.fi";
const TORI_BASE_URL: &str = "https://apps-gw-poc.svc.tori.fi";
const LOGIN_SERVICE: &str = "LOGIN-SERVER-AUTH";
const LOGIN_PATH: &str = "/public/login";
const GATEWAY_HMAC_KEY: &[u8] = b"3b535f36-79be-424b-a6fd-116c6e69f137";
const SCHIBSTED_USER_AGENT: &str = "user-webflows-sdk-android/5.0.0";
const TORI_USER_AGENT: &str = "ToriApp_iOS/26.16.0-26903";
const TORI_BEARER_LIFETIME_SECS: u64 = 60 * 60;
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_AUTH_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CALLBACK_URL_BYTES: usize = 8 * 1024;
const MAX_AUTHORIZATION_CODE_BYTES: usize = 4 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    fn new(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    #[doc(hidden)]
    pub fn new_for_adapter(value: String) -> Self {
        Self::new(value)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthFlow {
    pub flow_id: String,
    pub expires_at_unix: u64,
    pub(crate) state: SecretString,
    pub(crate) nonce: SecretString,
    pub(crate) pkce_verifier: SecretString,
    pub device_id: String,
    pub installation_id: String,
    pub ab_test_device_id: String,
}

impl std::fmt::Debug for OAuthFlow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthFlow")
            .field("flow_id", &self.flow_id)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("state", &self.state)
            .field("nonce", &self.nonce)
            .field("pkce_verifier", &self.pkce_verifier)
            .field("device_id", &"<redacted>")
            .field("installation_id", &"<redacted>")
            .field("ab_test_device_id", &"<redacted>")
            .finish()
    }
}

impl OAuthFlow {
    #[cfg(test)]
    pub(crate) fn state_for_adapter(&self) -> &str {
        self.state.expose()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuthStart {
    pub flow_id: String,
    pub login_url: String,
    pub expires_at_unix: u64,
    pub completion_command: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthCredentials {
    pub user_id: String,
    pub(crate) refresh_token: SecretString,
    pub(crate) bearer_token: SecretString,
    pub bearer_expires_at_unix: u64,
    pub device_id: String,
    pub installation_id: String,
    pub ab_test_device_id: String,
}

impl std::fmt::Debug for AuthCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthCredentials")
            .field("user_id", &"<redacted>")
            .field("refresh_token", &self.refresh_token)
            .field("bearer_token", &self.bearer_token)
            .field("bearer_expires_at_unix", &self.bearer_expires_at_unix)
            .field("device_id", &"<redacted>")
            .field("installation_id", &"<redacted>")
            .field("ab_test_device_id", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedAccount {
    pub user_id: String,
}

#[derive(Clone, Debug)]
pub struct SchibstedTokens {
    access_token: SecretString,
    refresh_token: SecretString,
    id_token: SecretString,
}

impl SchibstedTokens {
    #[doc(hidden)]
    pub fn new_for_adapter(access_token: String, refresh_token: String, id_token: String) -> Self {
        Self {
            access_token: SecretString::new(access_token),
            refresh_token: SecretString::new(refresh_token),
            id_token: SecretString::new(id_token),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToriSession {
    user_id: String,
    bearer_token: SecretString,
}

impl ToriSession {
    #[doc(hidden)]
    pub fn new_for_adapter(user_id: String, bearer_token: String) -> Self {
        Self {
            user_id,
            bearer_token: SecretString::new(bearer_token),
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait AuthenticationApi: Send + Sync {
    async fn exchange_authorization_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<SchibstedTokens, AppError>;

    async fn exchange_spid_code(&self, access_token: &str) -> Result<SecretString, AppError>;

    async fn login_to_tori(
        &self,
        spid_code: &str,
        id_token: &str,
        device_id: &str,
        installation_id: &str,
        ab_test_device_id: &str,
    ) -> Result<ToriSession, AppError>;
}

pub trait GatewaySigner: Send + Sync {
    fn sign(&self, method: &str, path: &str, service: &str, body: &[u8]) -> SecretString;
}

#[derive(Clone, Default)]
pub struct HmacGatewaySigner;

impl GatewaySigner for HmacGatewaySigner {
    fn sign(&self, method: &str, path: &str, service: &str, body: &[u8]) -> SecretString {
        let mut mac = Hmac::<Sha512>::new_from_slice(GATEWAY_HMAC_KEY)
            .expect("the static HMAC key has a valid length");
        mac.update(method.to_ascii_uppercase().as_bytes());
        mac.update(b";");
        if path != "/" {
            mac.update(path.as_bytes());
        }
        mac.update(b";");
        mac.update(service.as_bytes());
        mac.update(b";");
        mac.update(body);
        SecretString::new(
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()),
        )
    }
}

#[derive(Clone)]
pub struct SchibstedToriAuthenticationApi<S = HmacGatewaySigner> {
    client: reqwest::Client,
    signer: S,
    login_base_url: String,
    tori_base_url: String,
}

impl SchibstedToriAuthenticationApi<HmacGatewaySigner> {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .expect("static authentication client configuration is valid");
        Self::with_signer(client, HmacGatewaySigner)
    }
}

impl Default for SchibstedToriAuthenticationApi<HmacGatewaySigner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> SchibstedToriAuthenticationApi<S> {
    fn with_signer(client: reqwest::Client, signer: S) -> Self {
        Self {
            client,
            signer,
            login_base_url: LOGIN_BASE_URL.to_owned(),
            tori_base_url: TORI_BASE_URL.to_owned(),
        }
    }

    #[cfg(test)]
    fn with_base_urls(mut self, login_base_url: String, tori_base_url: String) -> Self {
        self.login_base_url = login_base_url;
        self.tori_base_url = tori_base_url;
        self
    }
}

impl<S: GatewaySigner> SchibstedToriAuthenticationApi<S> {
    pub async fn refresh_credentials(
        &self,
        refresh_token: &str,
        device_id: &str,
        installation_id: &str,
        ab_test_device_id: &str,
        now_unix: u64,
    ) -> Result<AuthCredentials, AppError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            refresh_token: String,
            id_token: String,
        }

        let response = self
            .client
            .post(format!("{}/oauth/token", self.login_base_url))
            .header("X-OIDC", "v1")
            .header("User-Agent", SCHIBSTED_USER_AGENT)
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .timeout(AUTH_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| upstream_error("token_refresh", true))?;
        ensure_success(response.status(), "token_refresh")?;
        let tokens: TokenResponse = bounded_json(response, "token_refresh").await?;
        if tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
            || tokens.id_token.is_empty()
        {
            return Err(unexpected_response("token_refresh"));
        }
        let spid = self.exchange_spid_code(&tokens.access_token).await?;
        let session = self
            .login_to_tori(
                spid.expose(),
                &tokens.id_token,
                device_id,
                installation_id,
                ab_test_device_id,
            )
            .await?;
        Ok(AuthCredentials {
            user_id: session.user_id,
            refresh_token: SecretString::new(tokens.refresh_token),
            bearer_token: session.bearer_token,
            bearer_expires_at_unix: now_unix.checked_add(TORI_BEARER_LIFETIME_SECS).ok_or_else(
                || AppError::authentication("auth.clock_invalid", "credential expiry is invalid"),
            )?,
            device_id: device_id.to_owned(),
            installation_id: installation_id.to_owned(),
            ab_test_device_id: ab_test_device_id.to_owned(),
        })
    }
}

impl<S: GatewaySigner> AuthenticationApi for SchibstedToriAuthenticationApi<S> {
    async fn exchange_authorization_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<SchibstedTokens, AppError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            refresh_token: String,
            id_token: String,
        }

        let response = self
            .client
            .post(format!("{}/oauth/token", self.login_base_url))
            .header("X-OIDC", "v1")
            .header("User-Agent", SCHIBSTED_USER_AGENT)
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", REDIRECT_URI),
                ("code_verifier", pkce_verifier),
            ])
            .timeout(AUTH_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| upstream_error("token_exchange", true))?;
        ensure_success(response.status(), "token_exchange")?;
        let tokens: TokenResponse = bounded_json(response, "token_exchange").await?;
        if tokens.access_token.is_empty()
            || tokens.refresh_token.is_empty()
            || tokens.id_token.is_empty()
        {
            return Err(unexpected_response("token_exchange"));
        }

        Ok(SchibstedTokens {
            access_token: SecretString::new(tokens.access_token),
            refresh_token: SecretString::new(tokens.refresh_token),
            id_token: SecretString::new(tokens.id_token),
        })
    }

    async fn exchange_spid_code(&self, access_token: &str) -> Result<SecretString, AppError> {
        #[derive(Deserialize)]
        struct ExchangeData {
            code: String,
        }
        #[derive(Deserialize)]
        struct ExchangeResponse {
            data: ExchangeData,
        }

        let response = self
            .client
            .post(format!("{}/api/2/oauth/exchange", self.login_base_url))
            .bearer_auth(access_token)
            .header("User-Agent", "AccountSDKIOSWeb/7.0.2 (iPhone; iOS 26.1)")
            .form(&[("clientId", EXCHANGE_CLIENT_ID), ("type", "code")])
            .timeout(AUTH_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| upstream_error("spid_exchange", true))?;
        ensure_success(response.status(), "spid_exchange")?;
        let exchange: ExchangeResponse = bounded_json(response, "spid_exchange").await?;
        if exchange.data.code.is_empty() {
            return Err(unexpected_response("spid_exchange"));
        }
        Ok(SecretString::new(exchange.data.code))
    }

    async fn login_to_tori(
        &self,
        spid_code: &str,
        id_token: &str,
        device_id: &str,
        installation_id: &str,
        ab_test_device_id: &str,
    ) -> Result<ToriSession, AppError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct LoginRequest<'a> {
            device_id: &'a str,
            id_token: &'a str,
            spid_code: &'a str,
        }
        #[derive(Deserialize)]
        struct LoginToken {
            value: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LoginResponse {
            user_id: serde_json::Value,
            token: LoginToken,
        }

        let body = serde_json::to_vec(&LoginRequest {
            device_id,
            id_token,
            spid_code,
        })
        .map_err(|_| unexpected_response("tori_login"))?;
        let signature = self.signer.sign("POST", LOGIN_PATH, LOGIN_SERVICE, &body);
        let response = self
            .client
            .post(format!("{}{}", self.tori_base_url, LOGIN_PATH))
            .header("User-Agent", TORI_USER_AGENT)
            .header("Accept", "application/json; charset=UTF-8")
            .header("Content-Type", "application/json")
            .header("finn-gw-service", LOGIN_SERVICE)
            .header("finn-gw-key", signature.expose())
            .header("finn-app-installation-id", installation_id)
            .header("ab-test-device-id", ab_test_device_id)
            .body(body)
            .timeout(AUTH_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| upstream_error("tori_login", true))?;
        ensure_success(response.status(), "tori_login")?;
        let login: LoginResponse = bounded_json(response, "tori_login").await?;
        let user_id = match login.user_id {
            serde_json::Value::String(value) if !value.is_empty() => value,
            serde_json::Value::Number(value) => value.to_string(),
            _ => return Err(unexpected_response("tori_login")),
        };
        if login.token.value.is_empty() {
            return Err(unexpected_response("tori_login"));
        }

        Ok(ToriSession {
            user_id,
            bearer_token: SecretString::new(login.token.value),
        })
    }
}

pub struct BrowserAuth<A> {
    api: A,
    flow_lifetime: Duration,
}

impl<A> BrowserAuth<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            flow_lifetime: DEFAULT_FLOW_LIFETIME,
        }
    }

    pub fn start(&self, now_unix: u64) -> Result<(OAuthFlow, AuthStart), AppError> {
        self.start_with_lifetime(now_unix, self.flow_lifetime)
    }

    fn start_with_lifetime(
        &self,
        now_unix: u64,
        lifetime: Duration,
    ) -> Result<(OAuthFlow, AuthStart), AppError> {
        let verifier = generate_pkce_verifier();
        let challenge = pkce_challenge(verifier.expose());
        let state = random_identifier();
        let nonce = random_identifier();
        let flow_id = Uuid::new_v4().to_string();
        let expires_at_unix = now_unix.checked_add(lifetime.as_secs()).ok_or_else(|| {
            AppError::new(
                "auth.clock_invalid",
                "authentication flow expiry is invalid",
                ExitClass::Authentication,
            )
        })?;

        let mut login_url = Url::parse(&format!("{LOGIN_BASE_URL}/oauth/authorize"))
            .expect("the static authorization URL is valid");
        login_url.query_pairs_mut().extend_pairs([
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("response_type", "code"),
            ("scope", "openid offline_access"),
            ("state", state.expose()),
            ("nonce", nonce.expose()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);

        let flow = OAuthFlow {
            flow_id: flow_id.clone(),
            expires_at_unix,
            state,
            nonce,
            pkce_verifier: verifier,
            device_id: Uuid::new_v4().to_string(),
            installation_id: Uuid::new_v4().to_string(),
            ab_test_device_id: Uuid::new_v4().to_string(),
        };
        let output = AuthStart {
            completion_command: format!("tori auth complete {flow_id}"),
            flow_id,
            login_url: login_url.into(),
            expires_at_unix,
        };
        Ok((flow, output))
    }
}

impl<A: AuthenticationApi> BrowserAuth<A> {
    pub async fn complete(
        &self,
        flow: &OAuthFlow,
        callback_url: &str,
        now_unix: u64,
    ) -> Result<(AuthCredentials, AuthenticatedAccount), AppError> {
        if now_unix >= flow.expires_at_unix {
            return Err(restart_error(
                "auth.flow_expired",
                "the authentication flow expired",
            ));
        }
        let code = validate_callback(callback_url, flow.state.expose())?;
        let tokens = self
            .api
            .exchange_authorization_code(code.expose(), flow.pkce_verifier.expose())
            .await?;
        let spid_code = self
            .api
            .exchange_spid_code(tokens.access_token.expose())
            .await?;
        let session = self
            .api
            .login_to_tori(
                spid_code.expose(),
                tokens.id_token.expose(),
                &flow.device_id,
                &flow.installation_id,
                &flow.ab_test_device_id,
            )
            .await?;
        let bearer_expires_at_unix =
            now_unix
                .checked_add(TORI_BEARER_LIFETIME_SECS)
                .ok_or_else(|| {
                    AppError::new(
                        "auth.clock_invalid",
                        "credential expiry is invalid",
                        ExitClass::Authentication,
                    )
                })?;
        let account = AuthenticatedAccount {
            user_id: session.user_id.clone(),
        };
        let credentials = AuthCredentials {
            user_id: session.user_id,
            refresh_token: tokens.refresh_token,
            bearer_token: session.bearer_token,
            bearer_expires_at_unix,
            device_id: flow.device_id.clone(),
            installation_id: flow.installation_id.clone(),
            ab_test_device_id: flow.ab_test_device_id.clone(),
        };
        Ok((credentials, account))
    }
}

fn generate_pkce_verifier() -> SecretString {
    let mut random = [0_u8; 48];
    for chunk in random.chunks_exact_mut(16) {
        chunk.copy_from_slice(Uuid::new_v4().as_bytes());
    }
    SecretString::new(URL_SAFE_NO_PAD.encode(random))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_identifier() -> SecretString {
    let mut random = [0_u8; 32];
    for chunk in random.chunks_exact_mut(16) {
        chunk.copy_from_slice(Uuid::new_v4().as_bytes());
    }
    SecretString::new(URL_SAFE_NO_PAD.encode(random))
}

async fn bounded_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    stage: &'static str,
) -> Result<T, AppError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
    {
        return Err(unexpected_response(stage));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unexpected_response(stage))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_AUTH_RESPONSE_BYTES {
            return Err(unexpected_response(stage));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| unexpected_response(stage))
}

fn validate_callback(callback_url: &str, expected_state: &str) -> Result<SecretString, AppError> {
    if callback_url.len() > MAX_CALLBACK_URL_BYTES {
        return Err(invalid_callback());
    }
    let parsed = Url::parse(callback_url).map_err(|_| invalid_callback())?;
    if parsed.scheme() != CALLBACK_SCHEME
        || parsed.host_str() != Some("login")
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
    if oauth_error {
        return Err(restart_error(
            "auth.authorization_denied",
            "the identity provider did not authorize the login",
        ));
    }
    if !state
        .as_deref()
        .is_some_and(|actual_state| states_equal(actual_state, expected_state))
    {
        return Err(restart_error(
            "auth.state_mismatch",
            "the authentication callback state is invalid",
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

fn states_equal(actual: &str, expected: &str) -> bool {
    let mut expected_mac = Hmac::<Sha256>::new_from_slice(b"oauth-state-comparison")
        .expect("the static comparison key has a valid length");
    expected_mac.update(expected.as_bytes());
    let expected_tag = expected_mac.finalize().into_bytes();

    let mut actual_mac = Hmac::<Sha256>::new_from_slice(b"oauth-state-comparison")
        .expect("the static comparison key has a valid length");
    actual_mac.update(actual.as_bytes());
    actual_mac.verify_slice(&expected_tag).is_ok()
}

fn invalid_callback() -> AppError {
    restart_error(
        "auth.callback_invalid",
        "the authentication callback is invalid",
    )
}

fn restart_error(code: &str, message: &str) -> AppError {
    let mut error = AppError::new(code, message, ExitClass::Authentication);
    error.next_actions.push(NextAction {
        command: "tori auth start".to_owned(),
    });
    error
}

fn ensure_success(status: StatusCode, stage: &'static str) -> Result<(), AppError> {
    if status.is_success() {
        return Ok(());
    }
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(upstream_error(stage, true));
    }
    let mut error = AppError::new(
        "auth.exchange_rejected",
        "the authentication service rejected the login",
        ExitClass::Authentication,
    );
    error.details = Some(Box::new(serde_json::json!({
        "stage": stage,
        "status": status.as_u16()
    })));
    Err(error)
}

fn upstream_error(stage: &'static str, retryable: bool) -> AppError {
    let mut error = AppError::new(
        "auth.upstream_unavailable",
        "an authentication service is unavailable",
        ExitClass::Upstream,
    );
    error.retryable = retryable;
    error.details = Some(Box::new(serde_json::json!({ "stage": stage })));
    error
}

fn unexpected_response(stage: &'static str) -> AppError {
    let mut error = AppError::new(
        "upstream.unexpected_response",
        "an authentication service returned an unexpected response",
        ExitClass::Upstream,
    );
    error.details = Some(Box::new(serde_json::json!({ "stage": stage })));
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeApi {
        calls: Mutex<Vec<&'static str>>,
    }

    impl AuthenticationApi for FakeApi {
        async fn exchange_authorization_code(
            &self,
            _code: &str,
            _pkce_verifier: &str,
        ) -> Result<SchibstedTokens, AppError> {
            self.calls.lock().unwrap().push("token");
            Ok(SchibstedTokens {
                access_token: SecretString::new("access".into()),
                refresh_token: SecretString::new("refresh".into()),
                id_token: SecretString::new("id".into()),
            })
        }

        async fn exchange_spid_code(&self, _access_token: &str) -> Result<SecretString, AppError> {
            self.calls.lock().unwrap().push("spid");
            Ok(SecretString::new("spid".into()))
        }

        async fn login_to_tori(
            &self,
            _spid_code: &str,
            _id_token: &str,
            _device_id: &str,
            _installation_id: &str,
            _ab_test_device_id: &str,
        ) -> Result<ToriSession, AppError> {
            self.calls.lock().unwrap().push("login");
            Ok(ToriSession {
                user_id: "42".into(),
                bearer_token: SecretString::new("bearer".into()),
            })
        }
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn start_generates_distinct_cryptographic_material() {
        let auth = BrowserAuth::new(FakeApi::default());
        let (first, output) = auth.start(1_000).unwrap();
        let (second, _) = auth.start(1_000).unwrap();
        let url = Url::parse(&output.login_url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();

        assert_ne!(first.flow_id, second.flow_id);
        assert_ne!(first.state.expose(), second.state.expose());
        assert_ne!(first.nonce.expose(), second.nonce.expose());
        assert_ne!(first.pkce_verifier.expose(), second.pkce_verifier.expose());
        assert!((43..=128).contains(&first.pkce_verifier.expose().len()));
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(output.expires_at_unix, 1_600);
        let debug = format!("{first:?}");
        assert!(!debug.contains(first.pkce_verifier.expose()));
        assert!(!debug.contains(&first.device_id));
        assert!(!debug.contains(&first.installation_id));
        assert!(!debug.contains(&first.ab_test_device_id));
    }

    #[tokio::test]
    async fn complete_validates_then_runs_all_exchanges() {
        let auth = BrowserAuth::new(FakeApi::default());
        let (flow, _) = auth.start(1_000).unwrap();
        let callback = format!(
            "{CALLBACK_SCHEME}://login?code=authorization-code&state={}",
            flow.state.expose()
        );

        let (credentials, account) = auth.complete(&flow, &callback, 1_001).await.unwrap();

        assert_eq!(account.user_id, "42");
        assert_eq!(credentials.user_id, "42");
        assert_eq!(credentials.bearer_expires_at_unix, 4_601);
        assert_eq!(
            auth.api.calls.lock().unwrap().as_slice(),
            ["token", "spid", "login"]
        );
    }

    #[tokio::test]
    async fn complete_rejects_expired_flow_before_network_calls() {
        let auth = BrowserAuth::new(FakeApi::default());
        let (flow, _) = auth.start(1_000).unwrap();
        let error = auth.complete(&flow, "invalid", 1_600).await.unwrap_err();

        assert_eq!(error.code, "auth.flow_expired");
        assert!(auth.api.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn callback_fixture_requires_exact_scheme_target_state_and_single_code() {
        #[derive(serde::Deserialize)]
        struct CallbackCase {
            callback: String,
            valid: bool,
        }

        let cases: Vec<CallbackCase> =
            serde_json::from_str(include_str!("../../tests/fixtures/oauth/callbacks.json"))
                .unwrap();
        for case in cases {
            let result = validate_callback(&case.callback, "expected");
            assert_eq!(
                result.is_ok(),
                case.valid,
                "unexpected callback fixture result for {}",
                case.callback
            );
            if case.valid {
                assert_eq!(result.unwrap().expose(), "ok");
            }
        }
    }

    #[test]
    fn callback_rejects_an_overlong_authorization_code() {
        let callback = format!(
            "{CALLBACK_SCHEME}://login?code={}&state=expected",
            "x".repeat(MAX_AUTHORIZATION_CODE_BYTES + 1)
        );
        assert!(validate_callback(&callback, "expected").is_err());
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = SecretString::new("sensitive-value".into());
        assert_eq!(format!("{secret:?}"), "<redacted>");
    }

    #[test]
    fn gateway_signature_matches_known_vector() {
        let signature = HmacGatewaySigner.sign(
            "POST",
            "/public/login",
            "LOGIN-SERVER-AUTH",
            br#"{"deviceId":"device","idToken":"id","spidCode":"code"}"#,
        );
        assert_eq!(
            signature.expose(),
            "Aw2zCNu7AE+osoZMzrdsgUES2Bt/NB/eHco/NjUjWLbWxyfJu5ewT/PqDaLGusaEJSMCFJRUm77ICdQGg1W7TA=="
        );
    }

    #[test]
    fn test_only_endpoint_override_keeps_adapter_configurable() {
        let api = SchibstedToriAuthenticationApi::new()
            .with_base_urls("http://identity.test".into(), "http://tori.test".into());
        assert_eq!(api.login_base_url, "http://identity.test");
        assert_eq!(api.tori_base_url, "http://tori.test");
    }
}
