//! Optional shared-secret auth for the Butler HTTP surface.
//!
//! When `server.password` (or `BUTLER_PASSWORD` / `BUTLER_API_TOKEN`) is empty, the
//! server stays open (dev default). When set, sensitive routes require:
//! - `Authorization: Bearer <password>`
//! - `Authorization: Basic base64(username:password)`
//! - `X-Butler-Token: <password>`
//!
//! Unauthenticated `GET /mcp/health` still returns liveness (`status=ok`) but **not**
//! loaded roots / request ring. Full health needs auth when a password is configured.

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use serde_json::json;

use super::state::AppState;

/// True when a non-empty shared secret is configured.
pub fn password_required(state: &AppState) -> bool {
    state
        .settings
        .server
        .password
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn configured_password(state: &AppState) -> Option<&str> {
    state
        .settings
        .server
        .password
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn configured_username(state: &AppState) -> &str {
    let u = state.settings.server.username.trim();
    if u.is_empty() {
        "butler"
    } else {
        u
    }
}

/// Validate Authorization / X-Butler-Token against config.
pub fn request_authorized(state: &AppState, req: &Request<Body>) -> bool {
    let Some(password) = configured_password(state) else {
        return true;
    };
    let username = configured_username(state);

    if let Some(tok) = req
        .headers()
        .get("x-butler-token")
        .and_then(|v| v.to_str().ok())
    {
        if constant_time_eq(tok.trim(), password) {
            return true;
        }
    }

    let Some(auth) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let auth = auth.trim();
    if let Some(rest) = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
    {
        return constant_time_eq(rest.trim(), password);
    }
    if let Some(b64) = auth
        .strip_prefix("Basic ")
        .or_else(|| auth.strip_prefix("basic "))
    {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
            if let Ok(s) = String::from_utf8(bytes) {
                if let Some((user, pass)) = s.split_once(':') {
                    return user == username && constant_time_eq(pass, password);
                }
            }
        }
    }
    false
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    // Avoid short-circuit length leak for equal-length secrets; still fine for Alpha shared secret.
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn unauthorized_json() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer, Basic realm=\"butler\"")],
        Json(json!({
            "error": "unauthorized",
            "hint": "Set Authorization: Bearer <server.password> (or Basic username:password). Env: BUTLER_PASSWORD / BUTLER_API_TOKEN. MCP: same env on the mcp client.",
        })),
    )
        .into_response()
}

fn public_liveness() -> Response {
    Json(json!({
        "status": "ok",
        "auth_required": true,
        "hint": "Full /mcp/health requires Authorization when server.password is set",
    }))
    .into_response()
}

/// Axum middleware: gate all routes when password configured.
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !password_required(&state) {
        return next.run(request).await;
    }

    let path = request.uri().path();
    let authorized = request_authorized(&state, &request);

    // Liveness without leaking warehouse inventory.
    if !authorized && (path == "/mcp/health" || path == "/fingerprint") {
        if path == "/mcp/health" {
            return public_liveness();
        }
        return Json(json!({
            "auth_required": true,
            "error": "unauthorized",
        }))
        .into_response();
    }

    if !authorized {
        return unauthorized_json();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
