//! Inbound adapter: Axum 0.8 routes, Askama templates and HTMX 4 partials.

mod admin;
mod dsl;
mod parent;
mod public;
mod pupil;
mod session;
mod sysadmin;
mod teacher;

use askama::Template;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use infra::auth::Principal;
use infra::repos::Repos;
use infra::Db;
use kernel::{AppError, Clock, Role, SystemClock};
use notifications::application::Notifier;
use std::sync::Arc;
use tutoring::application::TextGenerator;

pub use session::Session;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub repos: Repos,
    pub ai: Arc<dyn TextGenerator>,
    pub notifier: Arc<dyn Notifier>,
    pub clock: Arc<dyn Clock>,
    /// Demo mode shows demo sign-in details and parent codes on screen.
    pub demo: bool,
    pub secure_cookies: bool,
}

impl AppState {
    pub fn new(db: Db, ai: Arc<dyn TextGenerator>, notifier: Arc<dyn Notifier>, demo: bool, secure_cookies: bool) -> Self {
        let repos = Repos::new(db.pool.clone());
        Self { db, repos, ai, notifier, clock: Arc::new(SystemClock), demo, secure_cookies }
    }
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.db.pool
    }
}

// ------------------------------------------------------------------ errors
pub struct WebError(pub AppError);

impl From<AppError> for WebError {
    fn from(e: AppError) -> Self {
        WebError(e)
    }
}
impl From<askama::Error> for WebError {
    fn from(e: askama::Error) -> Self {
        WebError(AppError::Infra(e.to_string()))
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    title: String,
    message: String,
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, title, message) = match &self.0 {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Page not found", "We couldn't find that page. It may have moved, or the link may be wrong.".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "You don't have access to this page", "Sign in with an account that has access, or go back to your home page.".to_string()),
            AppError::Invalid(m) => (StatusCode::UNPROCESSABLE_ENTITY, "Check and try again", m.clone()),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "Someone changed this at the same time", "Reload the page and try again.".to_string()),
            AppError::Infra(e) => {
                tracing::error!(error = %e, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "We couldn't do that", "Something went wrong on our side. Your work is saved. Try again in a moment.".to_string())
            }
        };
        let body = ErrorPage { title: title.into(), message }.render().unwrap_or_else(|_| title.to_string());
        (status, Html(body)).into_response()
    }
}

pub type WebResult<T> = Result<T, WebError>;

pub fn render<T: Template>(t: T) -> WebResult<Html<String>> {
    Ok(Html(t.render()?))
}

/// Redirect that also works for HTMX requests (HX-Redirect).
pub fn go(to: &str) -> Response {
    let mut r = (StatusCode::SEE_OTHER, [(header::LOCATION, to)]).into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert("HX-Redirect", v);
    }
    r
}

pub fn is_htmx(h: &HeaderMap) -> bool {
    h.contains_key("HX-Request")
}

// ------------------------------------------------------------------ page context
#[derive(Clone)]
pub struct Nav {
    pub href: String,
    pub label: String,
    pub current: bool,
    pub badge: String,
}

#[derive(Clone)]
pub struct Ctx {
    pub title: String,
    pub user: String,
    pub role_label: String,
    pub school: String,
    pub nav: Vec<Nav>,
    pub demo: bool,
    pub switch: Vec<(String, String)>,
    pub staff: bool,
}

pub async fn ctx(st: &AppState, p: &Principal, title: &str, current: &str) -> WebResult<Ctx> {
    let mut nav: Vec<(&str, &str)> = Vec::new();
    match p.role {
        Role::Student => nav.extend([("/home", "Today"), ("/plan", "My plan"), ("/books", "My books"), ("/tests", "Practice tests")]),
        Role::Parent => nav.extend([("/family", "This week")]),
        Role::Teacher => nav.extend([("/teach", "My classes")]),
        Role::SchoolAdmin => nav.extend([("/school", "School"), ("/school/pupils", "Pupils and logins"), ("/school/parents", "Parents"), ("/teach", "Classes")]),
        Role::SystemAdmin => nav.extend([("/platform", "Schools"), ("/platform/content", "Content and AI")]),
    }
    let mut badge_for_dsl = String::new();
    if p.is_dsl() {
        if let Some(s) = p.school_id {
            let open: i64 = sqlx::query_scalar("SELECT count(*) FROM safeguarding.cases WHERE school_id = $1 AND status = 'open'")
                .bind(s.0)
                .fetch_one(st.pool())
                .await
                .unwrap_or(0);
            if open > 0 {
                badge_for_dsl = format!("{open} new");
            }
        }
        nav.push(("/safeguarding", "Safeguarding"));
    }
    let nav = nav
        .into_iter()
        .map(|(h, l)| Nav {
            href: h.into(),
            label: l.into(),
            current: current == h,
            badge: if h == "/safeguarding" { badge_for_dsl.clone() } else { String::new() },
        })
        .collect();
    let others = infra::auth::memberships_for_account(st.pool(), p.account_id).await?;
    let switch = others
        .into_iter()
        .filter(|o| o.membership_id != p.membership_id)
        .map(|o| (o.membership_id.to_string(), format!("{} at {}", o.role.label(), o.school_name)))
        .collect();
    Ok(Ctx {
        title: title.into(),
        user: p.display_name(),
        role_label: p.role.label().into(),
        school: p.school_name.clone(),
        nav,
        demo: st.demo,
        switch,
        staff: !matches!(p.role, Role::Student | Role::Parent),
    })
}

pub fn home_for(role: Role) -> &'static str {
    match role {
        Role::Student => "/home",
        Role::Parent => "/family",
        Role::Teacher => "/teach",
        Role::SchoolAdmin => "/school",
        Role::SystemAdmin => "/platform",
    }
}

// ------------------------------------------------------------------ security
/// CSRF defence: state-changing requests must come from this site (Origin or
/// Referer host equals Host). Session cookies are also SameSite=Lax.
async fn same_origin(req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let h = req.headers();
    let host = h.get("x-forwarded-host").or_else(|| h.get(header::HOST)).and_then(|v| v.to_str().ok()).unwrap_or("");
    let from = h
        .get(header::ORIGIN)
        .or_else(|| h.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(|u| u.split("://").nth(1))
        .map(|rest| rest.split('/').next().unwrap_or(""))
        .unwrap_or("");
    if host.is_empty() || from != host {
        tracing::warn!(host, from, "blocked cross-site request");
        return (StatusCode::FORBIDDEN, "This request came from another site and was blocked.").into_response();
    }
    next.run(req).await
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'"),
    );
    r
}

// ------------------------------------------------------------------ static files
const CSS: &str = include_str!("../static/style.css");
const HTMX: &str = include_str!("../static/htmx-4.0.0.min.js");
const APP_JS: &str = include_str!("../static/app.js");
const F400: &[u8] = include_bytes!("../static/fonts/atkinson-hyperlegible-next-latin-400-normal.woff2");
const F700: &[u8] = include_bytes!("../static/fonts/atkinson-hyperlegible-next-latin-700-normal.woff2");
const F800: &[u8] = include_bytes!("../static/fonts/atkinson-hyperlegible-next-latin-800-normal.woff2");
const FMONO: &[u8] = include_bytes!("../static/fonts/atkinson-hyperlegible-mono-latin-400-normal.woff2");

async fn static_file(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    let cache = (header::CACHE_CONTROL, "public, max-age=86400");
    match name.as_str() {
        "style.css" => ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), cache], CSS).into_response(),
        "htmx-4.0.0.min.js" => ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), cache], HTMX).into_response(),
        "app.js" => ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), cache], APP_JS).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn font(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    let bytes = match name.as_str() {
        "atkinson-hyperlegible-next-latin-400-normal.woff2" => F400,
        "atkinson-hyperlegible-next-latin-700-normal.woff2" => F700,
        "atkinson-hyperlegible-next-latin-800-normal.woff2" => F800,
        "atkinson-hyperlegible-mono-latin-400-normal.woff2" => FMONO,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response()
}

async fn healthz(State(st): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(st.pool()).await {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response(),
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(public::routes())
        .merge(pupil::routes())
        .merge(teacher::routes())
        .merge(admin::routes())
        .merge(parent::routes())
        .merge(dsl::routes())
        .merge(sysadmin::routes())
        .route("/healthz", get(healthz))
        .route("/static/{name}", get(static_file))
        .route("/static/fonts/{name}", get(font))
        .fallback(|| async { WebError(AppError::NotFound).into_response() })
        .layer(middleware::from_fn(same_origin))
        .layer(middleware::from_fn(security_headers))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(state: AppState, port: u16) -> anyhow::Result<()> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "web listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

