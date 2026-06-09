use askama::Template;
use axum::extract::{Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;
use sqlx::PgPool;
use tower_sessions::Session;

use crate::handlers::moderator::MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY;
use crate::handlers::notifications::{HeaderNotificationView, load_header_notifications};
use crate::routes::render_template_response;
use crate::handlers::user::{load_is_moderator, local_profile_domain};
use crate::handlers::user::MODERATOR_TEAM_USERNAME;
use crate::security::auth::{invalidate_remember_token_from_headers, clear_remember_cookie_value};

#[derive(Deserialize)]
pub struct LoginPageQuery {
    pub email_status: Option<String>,
}


#[derive(Template)]
#[template(path = "main/login.html")]
#[allow(dead_code)]
pub struct LoginTemplate {
    title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    pub username: String,
    pub email_status_message: String,
    pub email_status_success: bool,
}

pub async fn login(
    Query(query): Query<LoginPageQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let (email_status_message, email_status_success) = match query.email_status.as_deref() {
        Some("sent") => (
            "Registration successful. Check your email for the verification link.".to_string(),
            true,
        ),
        Some("verified") => ("Email verified. You can now sign in.".to_string(), true),
        Some("expired") => (
            "Your verification link has expired. Please sign up again.".to_string(),
            false,
        ),
        Some("invalid") => ("Invalid verification link.".to_string(), false),
        _ => ("".to_string(), false),
    };

    let template = LoginTemplate {
        title: "Login - Instavox".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        username: session_string(&session, "username", "").await,
        email_status_message,
        email_status_success,
    };
    render_template_response(&template)
}


#[derive(Template)]
#[template(path = "main/register.html")]
#[allow(dead_code)]
pub struct RegisterTemplate {
    title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    username: String,
}

#[derive(Deserialize)]
pub struct RegisterPageQuery {
    pub invite: Option<String>,
    pub token: Option<String>,
}

pub async fn register(
    Query(query): Query<RegisterPageQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let invite_token_candidate = query
        .invite
        .as_deref()
        .or(query.token.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let template = RegisterTemplate {
        title: "Register - Instavox".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        username: session_string(&session, "username", "").await,
    };
    render_template_response(&template)
}

#[derive(sqlx::FromRow)]
struct SessionIdentityRow {
    id: i32,
    username: String,
    public_id: i64,
    profile_photo_url: String,
    profile_photo_style: String,
}

pub async fn session_user_id(session: &Session) -> Option<i32> {
    session.get::<i32>("id").await.ok().flatten()
}

pub async fn session_public_user_id(session: &Session) -> Option<i64> {
    session.get::<i64>("public_id").await.ok().flatten()
}

pub async fn session_string(session: &Session, key: &str, default: &str) -> String {
    session
        .get::<String>(key)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_string())
}


pub async fn logout(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = session.delete().await {
        tracing::warn!("logout session delete failed: {}", err);
    }
    invalidate_remember_token_from_headers(&pool, &headers).await;

    let mut response = Redirect::to("/login").into_response();
    if let Ok(cookie_header) = HeaderValue::from_str(&clear_remember_cookie_value()) {
        response.headers_mut().append(SET_COOKIE, cookie_header);
    }
    response
}



pub async fn session_i32_with_default(session: &Session, key: &str, fallback: i32) -> i32 {
    match session.get::<i32>(key).await {
        Ok(Some(value)) => value,
        Ok(None) => fallback,
        Err(err) => {
            tracing::warn!("failed to read session key '{}': {}", key, err);
            fallback
        }
    }
}


pub async fn is_acting_as_team_session(session: &Session) -> bool {
    let session_username = session_string(session, "username", "").await;
    if !session_username.eq_ignore_ascii_case(MODERATOR_TEAM_USERNAME) {
        return false;
    }

    let acting_original_user_id =
        session_i32_with_default(session, MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY, 0).await;
    acting_original_user_id > 0
}


pub async fn load_session_identity_row(pool: &PgPool, user_id: i32) -> Option<SessionIdentityRow> {
    sqlx::query_as::<_, SessionIdentityRow>(
        r#"
        SELECT
            id,
            username,
            public_id,
            COALESCE(NULLIF(profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(profile_photo_style, '') AS profile_photo_style
        FROM users
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}


pub async fn apply_session_identity(
    session: &Session,
    identity: &SessionIdentityRow,
) -> Result<(), String> {
    session
        .insert("id", identity.id)
        .await
        .map_err(|err| format!("failed to set session id: {}", err))?;
    session
        .insert("username", &identity.username)
        .await
        .map_err(|err| format!("failed to set session username: {}", err))?;
    session
        .insert("public_id", identity.public_id)
        .await
        .map_err(|err| format!("failed to set session public_id: {}", err))?;
    session
        .insert("profile_photo_url", &identity.profile_photo_url)
        .await
        .map_err(|err| format!("failed to set session profile_photo_url: {}", err))?;
    session
        .insert("profile_photo_style", &identity.profile_photo_style)
        .await
        .map_err(|err| format!("failed to set session profile_photo_style: {}", err))?;
    Ok(())
}