//! Session extractor: reads the session cookie and loads the signed-in person.

use crate::{go, AppState};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::Response;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use infra::auth::Principal;
use kernel::Role;

pub const COOKIE: &str = "margin_session";

pub struct Session(pub Principal);

impl FromRequestParts<AppState> for Session {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(COOKIE).map(|c| c.value().to_string());
        let principal = match token {
            Some(t) => infra::auth::principal_for_token(st.pool(), &t).await.ok().flatten(),
            None => None,
        };
        principal.map(Session).ok_or_else(|| go("/"))
    }
}

/// Optional session for public pages.
pub struct MaybeSession(pub Option<Principal>);

impl FromRequestParts<AppState> for MaybeSession {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        Ok(MaybeSession(Session::from_request_parts(parts, st).await.ok().map(|s| s.0)))
    }
}

impl Session {
    pub fn require(&self, roles: &[Role]) -> Result<(), crate::WebError> {
        if roles.contains(&self.0.role) {
            Ok(())
        } else {
            Err(crate::WebError(kernel::AppError::Forbidden))
        }
    }
}

pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .build()
}

pub fn clear_cookie() -> Cookie<'static> {
    Cookie::build((COOKIE, "")).path("/").max_age(time::Duration::ZERO).build()
}
