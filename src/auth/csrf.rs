//! Double-submit-cookie CSRF protection (05-security-and-privacy.md §2):
//! every state-changing session-cookie request must echo a header that
//! matches a cookie value only this origin could have set. Bearer-token
//! requests are exempt — CSRF is a cookie-specific threat.

use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};

pub const CSRF_COOKIE_NAME: &str = "ocl_csrf";
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";

fn is_bearer_request(req: &Request) -> bool {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "))
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub async fn csrf_protect(jar: CookieJar, req: Request, next: Next) -> Response {
    let existing = jar.get(CSRF_COOKIE_NAME).map(|c| c.value().to_string());

    if !is_bearer_request(&req) && is_mutating(req.method()) {
        let header_token = req
            .headers()
            .get(CSRF_HEADER_NAME)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let valid = matches!(
            (&existing, &header_token),
            (Some(c), Some(h)) if c == h && !c.is_empty()
        );
        if !valid {
            return (StatusCode::FORBIDDEN, "missing or invalid CSRF token").into_response();
        }
    }

    let response = next.run(req).await;

    // Only issue a fresh cookie when the caller didn't already have one —
    // an existing token stays valid for the life of the session cookie
    // it's paired with.
    if existing.is_some() {
        return response;
    }

    let cookie = Cookie::build((CSRF_COOKIE_NAME, super::token::generate()))
        .path("/")
        .secure(true)
        .same_site(SameSite::Strict)
        .http_only(false) // JS must read this to echo it back as a header
        .build();
    (jar.add(cookie), response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/mutate", post(|| async { "ok" }))
            .route("/read", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(csrf_protect))
    }

    #[tokio::test]
    async fn get_request_issues_a_csrf_cookie() {
        let response = app()
            .oneshot(Request::builder().uri("/read").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains(CSRF_COOKIE_NAME));
    }

    #[tokio::test]
    async fn post_without_csrf_token_is_rejected() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn post_with_matching_cookie_and_header_succeeds() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}=abc123"))
                    .header(CSRF_HEADER_NAME, "abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_with_mismatched_cookie_and_header_is_rejected() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}=abc123"))
                    .header(CSRF_HEADER_NAME, "different")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn bearer_authenticated_post_is_exempt() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mutate")
                    .header(header::AUTHORIZATION, "Bearer some-api-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
