use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Request, Response, StatusCode},
};
use tower_http::auth::{AsyncAuthorizeRequest, AsyncRequireAuthorizationLayer};
use tracing::warn;

pub struct AuthConfig {
    proxy_api_bearer_token: String,
    dashboard_basic_user: String,
    dashboard_basic_password: String,
}

impl AuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let proxy_api_bearer_token = required_env("PROXY_API_BEARER_TOKEN")?;
        let dashboard_basic_user = required_env("DASHBOARD_BASIC_USER")?;
        let dashboard_basic_password = required_env("DASHBOARD_BASIC_PASSWORD")?;

        Ok(Self {
            proxy_api_bearer_token,
            dashboard_basic_user,
            dashboard_basic_password,
        })
    }

    fn validate_proxy_bearer_header(&self, auth_header: &str) -> bool {
        let Some(token) = parse_bearer_token(auth_header) else {
            return false;
        };

        constant_time_eq_str(token, &self.proxy_api_bearer_token)
    }

    // Comment: validation of dashboard basic auth header (user/password comparison)
    fn validate_dashboard_basic_header(&self, auth_header: &str) -> bool {
        let Some((user, password)) = parse_basic_credentials(auth_header) else {
            return false;
        };

        constant_time_eq_str(&user, &self.dashboard_basic_user)
            && constant_time_eq_str(&password, &self.dashboard_basic_password)
    }

    #[cfg(test)]
    pub(crate) fn from_values(
        proxy_token: &str,
        dashboard_user: &str,
        dashboard_password: &str,
    ) -> Self {
        Self {
            proxy_api_bearer_token: proxy_token.to_string(),
            dashboard_basic_user: dashboard_user.to_string(),
            dashboard_basic_password: dashboard_password.to_string(),
        }
    }
}

// ── AsyncAuthorizeRequest implementations ──────────────────────────────────

#[derive(Clone)]
pub struct ProxyBearerAuth {
    config: Arc<AuthConfig>,
}

impl AsyncAuthorizeRequest<Body> for ProxyBearerAuth {
    type RequestBody = Body;
    type ResponseBody = Body;
    type Future =
        std::future::Ready<Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>;

    fn authorize(&mut self, request: Request<Body>) -> Self::Future {
        let authorized = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| self.config.validate_proxy_bearer_header(value));

        if !authorized {
            let uri = request.uri().to_string();
            warn!("proxy auth rejected for {uri}");
            return std::future::ready(Err(api_unauthorized_response(
                "invalid or missing bearer token",
            )));
        }

        std::future::ready(Ok(request))
    }
}

#[derive(Clone)]
pub struct DashboardBasicAuth {
    config: Arc<AuthConfig>,
}

impl AsyncAuthorizeRequest<Body> for DashboardBasicAuth {
    type RequestBody = Body;
    type ResponseBody = Body;
    type Future =
        std::future::Ready<Result<Request<Self::RequestBody>, Response<Self::ResponseBody>>>;

    fn authorize(&mut self, request: Request<Body>) -> Self::Future {
        let authorized = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| self.config.validate_dashboard_basic_header(value));

        if !authorized {
            let uri = request.uri().to_string();
            warn!("dashboard auth rejected for {uri}");
            return std::future::ready(Err(dashboard_unauthorized_response()));
        }

        std::future::ready(Ok(request))
    }
}

// ── Layer builders ─────────────────────────────────────────────────────────

pub fn proxy_auth_layer(
    config: Arc<AuthConfig>,
) -> AsyncRequireAuthorizationLayer<ProxyBearerAuth> {
    AsyncRequireAuthorizationLayer::new(ProxyBearerAuth { config })
}

pub fn dashboard_auth_layer(
    config: Arc<AuthConfig>,
) -> AsyncRequireAuthorizationLayer<DashboardBasicAuth> {
    AsyncRequireAuthorizationLayer::new(DashboardBasicAuth { config })
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn required_env(key: &str) -> Result<String, String> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) => Err(format!("{key} is set but empty")),
        Err(_) => Err(format!("Missing required env var: {key}")),
    }
}

pub fn parse_bearer_token(auth_header: &str) -> Option<&str> {
    let token = auth_header.strip_prefix("Bearer ")?;
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }

    Some(token)
}

pub fn parse_basic_credentials(auth_header: &str) -> Option<(String, String)> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let encoded = auth_header.strip_prefix("Basic ")?;
    let decoded = STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;

    let (username, password) = decoded.split_once(':')?;
    if username.is_empty() || password.is_empty() {
        return None;
    }

    Some((username.to_string(), password.to_string()))
}

fn constant_time_eq_str(left: &str, right: &str) -> bool {
    use subtle::ConstantTimeEq;

    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn api_unauthorized_response(message: &str) -> Response<Body> {
    let payload = serde_json::json!({
        "error": "unauthorized",
        "message": message,
    })
    .to_string();

    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload))
        .expect("api unauthorized response should be valid")
}

fn dashboard_unauthorized_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"cerebrum-dashboard\"",
        )
        .body(Body::from("unauthorized"))
        .expect("dashboard unauthorized response should be valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_parse_bearer_token_accepts_valid_header() {
        let token = parse_bearer_token("Bearer token123");
        assert_eq!(token, Some("token123"));
    }

    #[test]
    fn auth_parse_bearer_token_rejects_invalid_shape() {
        assert_eq!(parse_bearer_token("bearer token123"), None);
        assert_eq!(parse_bearer_token("Bearer  token123"), None);
        assert_eq!(parse_bearer_token("Bearer"), None);
    }

    #[test]
    fn auth_parse_basic_credentials_accepts_valid_header() {
        let header = "Basic dXNlcjpwYXNz";
        let credentials = parse_basic_credentials(header);
        assert_eq!(credentials, Some(("user".to_string(), "pass".to_string())));
    }

    #[test]
    fn auth_parse_basic_credentials_rejects_malformed_values() {
        assert_eq!(parse_basic_credentials("Basic not-base64"), None);
        assert_eq!(parse_basic_credentials("Basic dXNlcnBhc3M="), None);
    }

    #[test]
    fn auth_validate_proxy_bearer_header_compares_in_constant_time_path() {
        let config = AuthConfig {
            proxy_api_bearer_token: "expected-token".to_string(),
            dashboard_basic_user: "user".to_string(),
            dashboard_basic_password: "pass".to_string(),
        };

        assert!(config.validate_proxy_bearer_header("Bearer expected-token"));
        assert!(!config.validate_proxy_bearer_header("Bearer wrong-token"));
    }

    #[test]
    fn auth_validate_dashboard_basic_header_compares_user_and_password() {
        let config = AuthConfig {
            proxy_api_bearer_token: "expected-token".to_string(),
            dashboard_basic_user: "user".to_string(),
            dashboard_basic_password: "pass".to_string(),
        };

        assert!(config.validate_dashboard_basic_header("Basic dXNlcjpwYXNz"));
        assert!(!config.validate_dashboard_basic_header("Basic dXNlcjp3cm9uZw=="));
    }

    #[test]
    fn auth_from_values_builds_config() {
        let config = AuthConfig::from_values("proxy-token", "user", "password");

        assert!(config.validate_proxy_bearer_header("Bearer proxy-token"));
        assert!(config.validate_dashboard_basic_header("Basic dXNlcjpwYXNzd29yZA=="));
    }
}
