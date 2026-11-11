//! Public pages: landing, sign in for pupils, families and staff, sign out, switch role.

use crate::session::{clear_cookie, session_cookie, MaybeSession, COOKIE};
use crate::{go, home_for, is_htmx, render, AppState, WebResult};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::cookie::CookieJar;
use infra::auth::{self, LoginError};
use kernel::{MembershipId, Role};
use serde::Deserialize;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(landing))
        .route("/signin", get(pupil_page))
        .route("/signin/school", post(pupil_school))
        .route("/signin/pupil", post(pupil_signin))
        .route("/signin/staff", get(staff_page).post(staff_signin))
        .route("/signin/family", get(family_page))
        .route("/signin/family/code", post(family_code))
        .route("/signin/family/verify", post(family_verify))
        .route("/signout", post(signout))
        .route("/switch/{membership}", post(switch))
}

#[derive(Template)]
#[template(path = "landing.html")]
struct Landing {
    demo: bool,
    pupil_pw: &'static str,
    staff_pw: &'static str,
    sysadmin_email: String,
}

async fn landing(State(st): State<AppState>, MaybeSession(p): MaybeSession) -> WebResult<Response> {
    if let Some(p) = p {
        return Ok(go(home_for(p.role)));
    }
    Ok(render(Landing {
        demo: st.demo,
        pupil_pw: infra::seed::DEMO_PUPIL_PASSWORD,
        staff_pw: infra::seed::DEMO_STAFF_PASSWORD,
        sysadmin_email: std::env::var("SYSADMIN_EMAIL").unwrap_or_else(|_| "admin@margin.example".into()),
    })?
    .into_response())
}

#[derive(Template)]
#[template(path = "signin_pupil.html")]
struct PupilPage {
    step2: bool,
    school_code: String,
    school_name: String,
    username: String,
    error: String,
}

#[derive(Template)]
#[template(path = "partials/pupil_step2.html")]
struct PupilStep2 {
    school_code: String,
    school_name: String,
    username: String,
    error: String,
}

async fn pupil_page() -> WebResult<Response> {
    Ok(render(PupilPage { step2: false, school_code: String::new(), school_name: String::new(), username: String::new(), error: String::new() })?.into_response())
}

#[derive(Deserialize)]
struct SchoolForm {
    school: String,
}

async fn pupil_school(State(st): State<AppState>, headers: HeaderMap, Form(f): Form<SchoolForm>) -> WebResult<Response> {
    let code = identity::domain::normalise_school_code(&f.school).unwrap_or_default();
    let school = if code.is_empty() { None } else { auth::school_by_code(st.pool(), &code).await? };
    let (step2, name, error) = match school {
        Some(s) if s.active => (true, s.name, String::new()),
        Some(_) => (false, String::new(), "This school isn't open on Margin yet. Ask your teacher when it starts.".to_string()),
        None => (false, String::new(), "We can't find that school code. Check your login card and try again.".to_string()),
    };
    if step2 && is_htmx(&headers) {
        return Ok(render(PupilStep2 { school_code: code, school_name: name, username: String::new(), error })?.into_response());
    }
    if !step2 && is_htmx(&headers) {
        // Re-render the first step inside #signin.
        let _ = name;
        return Ok(render(SchoolStep { school_code: f.school, error })?.into_response());
    }
    Ok(render(PupilPage { step2, school_code: code, school_name: name, username: String::new(), error })?.into_response())
}

/// The first step only, for HTMX re-renders.
#[derive(Template)]
#[template(path = "partials/school_step.html")]
struct SchoolStep {
    school_code: String,
    error: String,
}

#[derive(Deserialize)]
struct PupilForm {
    school: String,
    username: String,
    password: String,
}

async fn pupil_signin(State(st): State<AppState>, jar: CookieJar, Form(f): Form<PupilForm>) -> WebResult<Response> {
    let code = identity::domain::normalise_school_code(&f.school).unwrap_or_default();
    let Some(school) = auth::school_by_code(st.pool(), &code).await?.filter(|s| s.active) else {
        return Ok(go("/signin"));
    };
    let username = f.username.trim().to_lowercase();
    let error = match auth::student_login(st.pool(), school.id, &username, f.password.trim()).await? {
        Ok(m) => {
            let token = auth::create_session(st.pool(), m, Role::Student).await?;
            let jar = jar.add(session_cookie(token, st.secure_cookies));
            return Ok((jar, go("/home")).into_response());
        }
        Err(LoginError::Locked) => "Your account is locked after too many tries. Ask your teacher to reset your password.",
        Err(LoginError::SchoolInactive) => "This school isn't open on Margin yet.",
        Err(LoginError::Wrong) => "That username and password don't match. Check your card and try again.",
    };
    Ok(render(PupilPage { step2: true, school_code: school.code, school_name: school.name, username, error: error.into() })?.into_response())
}

#[derive(Template)]
#[template(path = "signin_staff.html")]
struct StaffPage {
    email: String,
    error: String,
}

async fn staff_page() -> WebResult<Response> {
    Ok(render(StaffPage { email: String::new(), error: String::new() })?.into_response())
}

#[derive(Deserialize)]
struct StaffForm {
    email: String,
    password: String,
}

async fn staff_signin(State(st): State<AppState>, jar: CookieJar, Form(f): Form<StaffForm>) -> WebResult<Response> {
    let email = f.email.trim().to_lowercase();
    let error = match auth::staff_login(st.pool(), &email, &f.password).await? {
        Ok(account) => {
            let all = auth::memberships_for_account(st.pool(), account).await?;
            // Prefer the most senior role first; the person can switch later.
            let order = |r: Role| match r {
                Role::SystemAdmin => 0,
                Role::SchoolAdmin => 1,
                Role::Teacher => 2,
                Role::Parent => 3,
                Role::Student => 4,
            };
            if let Some(p) = all.iter().filter(|p| !matches!(p.role, Role::Student | Role::Parent)).min_by_key(|p| order(p.role)) {
                let token = auth::create_session(st.pool(), p.membership_id, p.role).await?;
                let jar = jar.add(session_cookie(token, st.secure_cookies));
                return Ok((jar, go(home_for(p.role))).into_response());
            }
            "This account has no staff access. Parents sign in with an email code."
        }
        Err(LoginError::Locked) => "This account is locked after too many tries. Ask your school admin to unlock it.",
        Err(LoginError::SchoolInactive) => "Your school isn't open on Margin yet.",
        Err(LoginError::Wrong) => "That email and password don't match.",
    };
    Ok(render(StaffPage { email, error: error.into() })?.into_response())
}

#[derive(Template)]
#[template(path = "signin_family.html")]
struct FamilyPage {
    code_step: bool,
    email: String,
    demo_code: String,
    error: String,
}

async fn family_page() -> WebResult<Response> {
    Ok(render(FamilyPage { code_step: false, email: String::new(), demo_code: String::new(), error: String::new() })?.into_response())
}

#[derive(Deserialize)]
struct EmailForm {
    email: String,
}

async fn family_code(State(st): State<AppState>, Form(f): Form<EmailForm>) -> WebResult<Response> {
    let email = f.email.trim().to_lowercase();
    if !email.contains('@') {
        return Ok(render(FamilyPage { code_step: false, email, demo_code: String::new(), error: "Enter an email address, like name@example.com.".into() })?.into_response());
    }
    let code = auth::issue_parent_code(st.pool(), &email).await?;
    let mut demo_code = String::new();
    if let Some(c) = code {
        let _ = st.notifier.email(&email, "Your Margin sign-in code", &format!("Your code is {c}. It works for 10 minutes.")).await;
        if st.demo {
            demo_code = c;
        }
    }
    // Same screen whether or not the address is known, so it can't be used to find accounts.
    Ok(render(FamilyPage { code_step: true, email, demo_code, error: String::new() })?.into_response())
}

#[derive(Deserialize)]
struct VerifyForm {
    email: String,
    code: String,
}

async fn family_verify(State(st): State<AppState>, jar: CookieJar, Form(f): Form<VerifyForm>) -> WebResult<Response> {
    let email = f.email.trim().to_lowercase();
    let code: String = f.code.chars().filter(|c| c.is_ascii_digit()).collect();
    match auth::verify_parent_code(st.pool(), &email, &code).await? {
        Some(m) => {
            let token = auth::create_session(st.pool(), m, Role::Parent).await?;
            let jar = jar.add(session_cookie(token, st.secure_cookies));
            Ok((jar, go("/family")).into_response())
        }
        None => Ok(render(FamilyPage {
            code_step: true,
            email,
            demo_code: String::new(),
            error: "That code is wrong or has expired. Check the email or send a new code.".into(),
        })?
        .into_response()),
    }
}

async fn signout(State(st): State<AppState>, jar: CookieJar) -> WebResult<Response> {
    if let Some(c) = jar.get(COOKIE) {
        auth::end_session(st.pool(), c.value()).await?;
    }
    Ok((jar.add(clear_cookie()), go("/")).into_response())
}

async fn switch(State(st): State<AppState>, jar: CookieJar, Path(to): Path<MembershipId>) -> WebResult<Response> {
    let Some(token) = jar.get(COOKIE).map(|c| c.value().to_string()) else { return Ok(go("/")) };
    let Some(me) = auth::principal_for_token(st.pool(), &token).await? else { return Ok(go("/")) };
    auth::switch_session(st.pool(), &token, to, me.account_id).await?;
    let p = auth::principal(st.pool(), to).await?;
    Ok(go(home_for(p.role)))
}
