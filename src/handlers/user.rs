use askama::Template;
use axum::{
    Form,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode, header::REFERER},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
pub use sqlx::PgPool;
use tokio::fs;
use url::form_urlencoded::byte_serialize;

use crate::{CANONICAL_INSTAVOX_DOMAIN, handlers::image_processing::{compress_upload_to_jpeg, read_uploaded_image}};
use crate::handlers::notifications::{HeaderNotificationView, load_header_notifications};
use crate::handlers::post::{FeedPostRow, IndexPostView, build_feed_post_views};
use crate::security::auth::{
    generate_actor_keypair, hash_password, is_valid_signup_email, secure_password_requirements_text,
    validate_secure_password, verify_password,
};


const MAX_PROFILE_BIO_LENGTH: usize = 2_000;

pub const DEFAULT_PROFILE_PHOTO_URL: &str = "/public/avatar.webp";
const DEFAULT_BACKGROUND_PHOTO_URL: &str = "/public/pexels-enginakyurt-17902901.webp";
pub const MODERATOR_TEAM_USERNAME: &str = "instavox-team";
const MODERATOR_TEAM_PREFERRED_USERNAME: &str = "instavox-team";
const MODERATOR_TEAM_EMAIL: &str = "team@instavox.social";
const MAX_SETTINGS_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const COMPRESSED_IMAGE_EXTENSION: &str = "jpg";



#[derive(sqlx::FromRow)]
struct ProfileFriendRow {
    user_id: i32,
    public_id: i64,
    username: String,
    profile_photo_url: String,
    profile_photo_style: String,
}

#[derive(sqlx::FromRow)]
struct UploadedUserImageRow {
    upload_id: i64,
    media_type: String,
    file_url: String,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct RemoteProfileUserRow {
    actor_id: String,
    preferred_username: String,
    display_name: String,
    summary: String,
    icon_url: String,
    host: String,
}

pub struct ProfileFriendView {
    pub public_id: i64,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
}

pub fn public_base_url() -> String {
    std::env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| format!("https://{}", local_profile_domain()))
}

pub fn local_profile_domain() -> String {
    std::env::var("LOCAL_PROFILE_DOMAIN").unwrap_or_else(|_| "instavox.social".to_string())
}

pub fn local_profile_domain_matches(candidate: &str) -> bool {
    candidate.eq_ignore_ascii_case(&local_profile_domain())
}

pub fn local_user_profile_path(username: &str) -> String {
    format!("/user/{}@{}", username.trim(), local_profile_domain())
}

pub fn redirect_back_path(headers: &HeaderMap) -> String {
    headers
        .get(REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split("://").nth(1).map(|tail| tail.to_string()))
        .and_then(|value| value.find('/').map(|idx| value[idx..].to_string()))
        .filter(|value| value.starts_with('/') && !value.starts_with("//"))
        .unwrap_or_else(|| "/settings".to_string())
}

pub fn crop_style_from_form(form: &SelectUploadedImageForm) -> String {
    if form.crop_apply.as_deref().unwrap_or_default().is_empty() {
        return String::new();
    }

    let x = form.crop_x.as_deref().unwrap_or("50").trim();
    let y = form.crop_y.as_deref().unwrap_or("50").trim();
    let zoom = form.crop_zoom.as_deref().unwrap_or("1").trim();
    format!("object-position: {}% {}%; transform: scale({});", x, y, zoom)
}

pub fn detect_image_extension(file_name: &str, content_type: &str) -> Option<&'static str> {
    let lower_name = file_name.to_ascii_lowercase();
    let lower_content = content_type.to_ascii_lowercase();
    if lower_content.contains("png") || lower_name.ends_with(".png") {
        Some("png")
    } else if lower_content.contains("webp") || lower_name.ends_with(".webp") {
        Some("webp")
    } else if lower_content.contains("jpeg")
        || lower_content.contains("jpg")
        || lower_name.ends_with(".jpg")
        || lower_name.ends_with(".jpeg")
    {
        Some("jpg")
    } else {
        None
    }
}

pub fn normalize_uploaded_filename_stem(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name)
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(64)
        .collect::<String>()
}

pub async fn notify_profile_update_to_federation(_pool: &PgPool, _user_id: i32, _reason: &str) {}

pub fn build_local_actor_urls(username: &str) -> (String, String, String) {
    let encoded_username: String = byte_serialize(username.as_bytes()).collect();
    let base_url = public_base_url();
    let actor_id = format!("{}/ap/users/{}", base_url, encoded_username);
    let inbox = format!("{}/inbox", actor_id);
    let outbox = format!("{}/outbox", actor_id);
    (actor_id, inbox, outbox)
}


pub async fn allocate_unique_public_user_id(pool: &PgPool) -> Result<i64, sqlx::Error> {
    loop {
        let candidate = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT FLOOR(1000000000000000 + random() * 9000000000000000)::BIGINT
            "#,
        )
        .fetch_one(pool)
        .await?;

        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM users
                WHERE public_id = $1
            )
            "#,
        )
        .bind(candidate)
        .fetch_one(pool)
        .await?;

        if !exists {
            return Ok(candidate);
        }
    }
}


pub async fn ensure_moderator_team_user(pool: &PgPool) -> Result<i32, String> {
    if let Some(user_id) = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT id
        FROM users
        WHERE LOWER(username) = LOWER($1)
        LIMIT 1
        "#,
    )
    .bind(MODERATOR_TEAM_USERNAME)
    .fetch_optional(pool)
    .await
    .map_err(|err| format!("team user lookup failed: {}", err))?
    {
        if let Err(err) = sqlx::query(
            r#"
            UPDATE users
            SET role = 'mod'
            WHERE id = $1
              AND LOWER(COALESCE(role::TEXT, 'user')) NOT IN ('mod', 'moderator')
            "#,
        )
        .bind(user_id)
        .execute(pool)
        .await
        {
            tracing::warn!("team user role upgrade failed: {}", err);
        }
        return Ok(user_id);
    }

    let public_id = allocate_unique_public_user_id(pool)
        .await
        .map_err(|err| format!("team user public id allocation failed: {}", err))?;
    let password_hash = hash_password(&format!(
        "instavox-team-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ))
    .map_err(|err| format!("team user password hash failed: {}", err))?;
    let keypair = generate_actor_keypair()
        .map_err(|err| format!("team user keypair generation failed: {}", err))?;
    let (ap_id, ap_inbox, ap_outbox) = build_local_actor_urls(MODERATOR_TEAM_PREFERRED_USERNAME);

    match sqlx::query_scalar::<_, i32>(
        r#"
        INSERT INTO users (
            first_name,
            last_name,
            username,
            preferred_username,
            birthday,
            gender,
            email,
            password,
            profile_photo_url,
            background_photo_url,
            created_at,
            bio_description,
            role,
            ap_public_key,
            ap_private_key,
            ap_id,
            ap_inbox,
            ap_outbox,
            ap_local,
            federation_enabled,
            ap_last_refreshed_at,
            public_id,
            email_verified,
            email_verification_token,
            email_verification_sent_at,
            email_verification_expires_at,
            email_verified_at
        )
        VALUES (
            'Instavox',
            'Team',
            $1,
            $2,
            DATE '2000-09-01',
            'other',
            $3,
            $4,
            $5,
            $6,
            CURRENT_TIMESTAMP,
            '',
            'mod',
            $7,
            $8,
            $9,
            $10,
            $11,
            TRUE,
            FALSE,
            CURRENT_TIMESTAMP,
            $12,
            TRUE,
            NULL,
            NULL,
            NULL,
            CURRENT_TIMESTAMP
        )
        RETURNING id
        "#,
    )
    .bind(MODERATOR_TEAM_USERNAME)
    .bind(MODERATOR_TEAM_PREFERRED_USERNAME)
    .bind(MODERATOR_TEAM_EMAIL)
    .bind(password_hash)
    .bind(DEFAULT_PROFILE_PHOTO_URL)
    .bind(DEFAULT_BACKGROUND_PHOTO_URL)
    .bind(keypair.public_key)
    .bind(keypair.private_key)
    .bind(ap_id)
    .bind(ap_inbox)
    .bind(ap_outbox)
    .bind(public_id)
    .fetch_one(pool)
    .await
    {
        Ok(user_id) => Ok(user_id),
        Err(insert_err) => sqlx::query_scalar::<_, i32>(
            r#"
            SELECT id
            FROM users
            WHERE LOWER(username) = LOWER($1)
            LIMIT 1
            "#,
        )
        .bind(MODERATOR_TEAM_USERNAME)
        .fetch_optional(pool)
        .await
        .map_err(|lookup_err| {
            format!(
                "team user creation failed: {}; fallback lookup failed: {}",
                insert_err, lookup_err
            )
        })?
        .ok_or_else(|| format!("team user creation failed: {}", insert_err)),
    }
}


pub fn is_moderator_role(role: &str) -> bool {
    matches!(role, "mod" | "moderator")
}



pub async fn load_user_role(pool: &PgPool, user_id: i32) -> String {
    sqlx::query_scalar::<_, String>(
        r#"SELECT LOWER(COALESCE(role::TEXT, 'user')) FROM users WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "user".to_string())
}



pub async fn load_is_moderator(pool: &PgPool, user_id: Option<i32>) -> bool {
    match user_id {
        Some(user_id) => is_moderator_role(&load_user_role(pool, user_id).await),
        None => false,
    }
}



#[derive(Template)]
#[template(path = "models/user.html")]
#[allow(dead_code)]
pub struct UserTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_found: bool,
    pub profile_id: i32,
    pub profile_public_id: i64,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub viewed_profile_photo_url: String,
    pub viewed_profile_photo_style: String,
    pub profile_background_photo_url: String,
    pub profile_background_photo_style: String,
    pub profile_username: String,
    pub profile_display_name: String,
    pub has_visible_profile_name: bool,
    pub profile_birthday: String,
    pub can_view_profile_birthday: bool,
    pub profile_created_at: String,
    pub profile_first_name: String,
    pub profile_last_name: String,
    pub profile_email: String,
    pub profile_bio_description: String,
    pub is_instavox_team_profile: bool,
    pub is_remote_profile: bool,
    pub remote_profile_url: String,
    pub pending_requests: Vec<PendingFriendRequest>,
    pub incoming_requests: Vec<IncomingFriendRequestView>,
    pub outgoing_requests: Vec<OutgoingFriendRequestView>,
    pub friendships: Vec<RelationshipUser>,
    pub blocked_users: Vec<RelationshipUser>,
    pub has_friendship_with_profile: bool,
    pub has_incoming_request_from_profile: bool,
    pub incoming_request_id_for_profile: i32,
    pub has_outgoing_request_to_profile: bool,
    pub has_following_profile: bool,
    pub has_blocked_profile: bool,
    pub is_own_profile: bool,
    pub profile_posts: Vec<IndexPostView>,
    pub profile_friends: Vec<ProfileFriendView>,
    pub profile_uploaded_images: Vec<UploadedUserImageView>,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
}


#[derive(Template)]
#[template(path = "main/settings.html")]
#[allow(dead_code)]
pub struct SettingsTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub current_profile_photo_url: String,
    pub current_profile_photo_style: String,
    pub current_background_photo_url: String,
    pub current_background_photo_style: String,
    pub uploaded_profile_images: Vec<String>,
    pub uploaded_background_images: Vec<String>,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    pub username: String,
    pub password_message: String,
    pub password_success: bool,
    pub email_message: String,
    pub email_success: bool,
    pub current_email: String,
    pub federation_enabled: bool,
    pub federation_display_name_mode: String,
    pub federation_message: String,
    pub federation_success: bool,
    pub privacy_message: String,
    pub privacy_success: bool,
    pub first_name_public: bool,
    pub last_name_public: bool,
    pub birthday_public: bool,
    pub delete_message: String,
    pub delete_success: bool,
}


#[derive(sqlx::FromRow)]
pub struct ProfileUser {
    pub id: i32,
    pub public_id: i64,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub background_photo_url: String,
    pub background_photo_style: String,
    pub username: String,
    pub birthday: String,
    pub birthday_public: bool,
    pub created_at: String,
    pub first_name: String,
    pub first_name_public: bool,
    pub last_name: String,
    pub last_name_public: bool,
    pub email: String,
    pub bio_description: String,
}







fn settings_password_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "changed" => ("Password updated successfully.".to_string(), true),
        "current_invalid" => ("Current password is incorrect.".to_string(), false),
        "mismatch" => (
            "New password and confirmation do not match.".to_string(),
            false,
        ),
        "too_short" | "weak" => (secure_password_requirements_text(), false),
        "same" => (
            "New password must be different from your current password.".to_string(),
            false,
        ),
        "missing" => ("All password fields are required.".to_string(), false),
        "hash_error" => ("Unable to update password right now.".to_string(), false),
        _ => (String::new(), false),
    }
}



fn settings_email_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "changed" => ("Email updated successfully.".to_string(), true),
        "current_invalid" => ("Current password is incorrect.".to_string(), false),
        "mismatch" => (
            "New email and confirmation do not match.".to_string(),
            false,
        ),
        "same" => (
            "New email must be different from your current email.".to_string(),
            false,
        ),
        "invalid" => ("Please enter a valid email address.".to_string(), false),
        "taken" => ("This email is already in use.".to_string(), false),
        "missing" => ("All email fields are required.".to_string(), false),
        "update_error" => ("Unable to update email right now.".to_string(), false),
        _ => (String::new(), false),
    }
}



fn settings_federation_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "changed_enabled" => (
            "Federation is now enabled for your account.".to_string(),
            true,
        ),
        "changed_disabled" => (
            "Federation is now disabled for your account.".to_string(),
            true,
        ),
        "invalid" => ("Invalid federation setting value.".to_string(), false),
        "update_error" => (
            "Unable to update federation setting right now.".to_string(),
            false,
        ),
        _ => (String::new(), false),
    }
}



fn parse_federation_setting(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "enabled" | "true" | "1" | "yes" | "on" => Some(true),
        "disabled" | "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}



fn parse_federation_display_name_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "full_name" | "name" | "first_last" | "first_last_name" => Some("full_name"),
        "preferred_username" | "preferred" | "username" => Some("preferred_username"),
        _ => None,
    }
}









pub async fn user_redirect(session: Session, State(pool): State<PgPool>) -> impl IntoResponse {
    let Some(user_id) = session_user_id(&session).await else {
        return Redirect::to("/login");
    };

    let session_username = session_string(&session, "username", "").await;
    if !session_username.trim().is_empty() {
        return Redirect::to(&local_user_profile_path(&session_username));
    }

    if let Some(username) = load_username_by_user_id(&pool, user_id).await {
        if let Err(err) = session.insert("username", &username).await {
            tracing::warn!("failed to cache username in session: {}", err);
        }
        return Redirect::to(&local_user_profile_path(&username));
    }

    if let Some(public_id) = load_public_user_id(&pool, user_id).await {
        if let Err(err) = session.insert("public_id", public_id).await {
            tracing::warn!("failed to cache public_id in session: {}", err);
        }

        if let Some(username) = load_username_by_user_id(&pool, user_id).await {
            if let Err(err) = session.insert("username", &username).await {
                tracing::warn!("failed to cache username in session: {}", err);
            }
            return Redirect::to(&local_user_profile_path(&username));
        }
    }

    Redirect::to("/login")
}


pub async fn user(
    Path(user_lookup): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let requested_lookup = user_lookup.trim().to_string();
    let parsed_remote_lookup =
        if let Some((username_part, domain_part)) = requested_lookup.split_once('@') {
            let username_part = username_part.trim().trim_start_matches('@');
            let domain_part = domain_part.trim();
            if username_part.is_empty()
                || domain_part.is_empty()
                || local_profile_domain_matches(domain_part)
            {
                None
            } else {
                Some((username_part.to_string(), domain_part.to_ascii_lowercase()))
            }
        } else {
            None
        };
    let parsed_username_lookup =
        if let Some((username_part, domain_part)) = requested_lookup.split_once('@') {
            let username_part = username_part.trim();
            let domain_part = domain_part.trim();
            if username_part.is_empty() || domain_part.is_empty() {
                None
            } else if local_profile_domain_matches(domain_part) {
                Some(username_part.to_string())
            } else {
                Some(String::new())
            }
        } else if requested_lookup.parse::<i64>().is_ok() {
            None
        } else if requested_lookup.is_empty() {
            None
        } else {
            Some(requested_lookup.clone())
        };

    let profile_result = if let Some(username_lookup) = parsed_username_lookup {
        if username_lookup.is_empty() {
            Ok(None)
        } else {
            sqlx::query_as::<_, ProfileUser>(
                r#"
                SELECT
                    id,
                    public_id,
                    COALESCE(NULLIF(profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                    COALESCE(profile_photo_style, '') AS profile_photo_style,
                    COALESCE(NULLIF(background_photo_url, ''), '/public/pexels-enginakyurt-17902901.webp') AS background_photo_url,
                    COALESCE(background_photo_style, '') AS background_photo_style,
                    username,
                    COALESCE(TO_CHAR(birthday, 'YYYY-MM-DD'), '') AS birthday,
                    COALESCE(birthday_public, TRUE) AS birthday_public,
                    COALESCE(TO_CHAR(created_at, 'YYYY-MM-DD'), '') AS created_at,
                    first_name,
                    COALESCE(first_name_public, TRUE) AS first_name_public,
                    last_name,
                    COALESCE(last_name_public, TRUE) AS last_name_public,
                    email,
                    COALESCE(bio_description, '') AS bio_description
                FROM users
                WHERE LOWER(username) = LOWER($1)
                LIMIT 1
                "#,
            )
            .bind(username_lookup)
            .fetch_optional(&pool)
            .await
        }
    } else if let Ok(public_user_id) = requested_lookup.parse::<i64>() {
        sqlx::query_as::<_, ProfileUser>(
            r#"
            SELECT
                id,
                public_id,
                COALESCE(NULLIF(profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                COALESCE(profile_photo_style, '') AS profile_photo_style,
                COALESCE(NULLIF(background_photo_url, ''), '/public/pexels-enginakyurt-17902901.webp') AS background_photo_url,
                COALESCE(background_photo_style, '') AS background_photo_style,
                username,
                COALESCE(TO_CHAR(birthday, 'YYYY-MM-DD'), '') AS birthday,
                COALESCE(birthday_public, TRUE) AS birthday_public,
                COALESCE(TO_CHAR(created_at, 'YYYY-MM-DD'), '') AS created_at,
                first_name,
                COALESCE(first_name_public, TRUE) AS first_name_public,
                last_name,
                COALESCE(last_name_public, TRUE) AS last_name_public,
                email,
                COALESCE(bio_description, '') AS bio_description
            FROM users
            WHERE public_id = $1
            LIMIT 1
            "#,
        )
        .bind(public_user_id)
        .fetch_optional(&pool)
        .await
    } else {
        Ok(None)
    };

    let remote_profile_result =
        if let Some((remote_username, remote_domain)) = &parsed_remote_lookup {
            sqlx::query_as::<_, RemoteProfileUserRow>(
                r#"
            SELECT
                actor_id,
                preferred_username,
                display_name,
                summary,
                COALESCE(NULLIF(icon_url, ''), '/public/avatar.webp') AS icon_url,
                COALESCE(NULLIF(host, ''), $2) AS host
            FROM ap_remote_actor
            WHERE LOWER(COALESCE(NULLIF(status, ''), 'discovered')) = 'discovered'
              AND LOWER(preferred_username) = LOWER($1)
              AND (
                    LOWER(COALESCE(NULLIF(host, ''), '')) = LOWER($2)
                 OR actor_id ILIKE ('https://' || $2 || '/%')
                 OR actor_id ILIKE ('http://' || $2 || '/%')
              )
            LIMIT 1
            "#,
            )
            .bind(remote_username)
            .bind(remote_domain)
            .fetch_optional(&pool)
            .await
        } else {
            Ok(None)
        };

    let (
        profile_found,
        profile_id,
        profile_public_id,
        viewed_profile_photo_url,
        viewed_profile_photo_style,
        profile_background_photo_url,
        profile_background_photo_style,
        profile_username,
        profile_birthday,
        profile_birthday_public,
        profile_created_at,
        profile_first_name,
        profile_first_name_public,
        profile_last_name,
        profile_last_name_public,
        profile_email,
        profile_bio_description,
        is_remote_profile,
        remote_profile_url,
    ) = match (profile_result, remote_profile_result) {
        (Ok(Some(profile)), _) => (
            true,
            profile.id,
            profile.public_id,
            profile.profile_photo_url,
            profile.profile_photo_style,
            profile.background_photo_url,
            profile.background_photo_style,
            profile.username,
            profile.birthday,
            profile.birthday_public,
            profile.created_at,
            profile.first_name,
            profile.first_name_public,
            profile.last_name,
            profile.last_name_public,
            profile.email,
            profile.bio_description,
            false,
            String::new(),
        ),
        (Ok(None), Ok(Some(remote_profile))) => {
            let handle = format!(
                "{}@{}",
                remote_profile.preferred_username.trim(),
                remote_profile.host.trim()
            );
            (
                true,
                0,
                0,
                remote_profile.icon_url,
                String::new(),
                "/public/pexels-enginakyurt-17902901.webp".to_string(),
                String::new(),
                handle,
                String::new(),
                false,
                String::new(),
                remote_profile.display_name,
                true,
                String::new(),
                false,
                String::new(),
                remote_profile.summary,
                true,
                remote_profile.actor_id,
            )
        }
        (Ok(None), Ok(None)) => (
            false,
            0,
            0,
            "/public/avatar.webp".to_string(),
            String::new(),
            "/public/pexels-enginakyurt-17902901.webp".to_string(),
            String::new(),
            String::new(),
            String::new(),
            false,
            String::new(),
            String::new(),
            false,
            String::new(),
            false,
            String::new(),
            String::new(),
            false,
            String::new(),
        ),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!("failed to load profile '{}': {}", requested_lookup, err);
            (
                false,
                0,
                0,
                "/public/avatar.webp".to_string(),
                String::new(),
                "/public/pexels-enginakyurt-17902901.webp".to_string(),
                String::new(),
                String::new(),
                String::new(),
                false,
                String::new(),
                String::new(),
                false,
                String::new(),
                false,
                String::new(),
                String::new(),
                false,
                String::new(),
            )
        }
    };

    let title = if profile_found {
        format!("@{} on Instavox", profile_username)
    } else {
        "User not found - Instavox".to_string()
    };
    let is_instavox_team_profile = profile_found
        && !is_remote_profile
        && profile_username.eq_ignore_ascii_case(MODERATOR_TEAM_USERNAME);

    let (pending_requests, incoming_requests, outgoing_requests, friendships, blocked_users) =
        if let Some(current_user_id) = current_user_id {
            let relationship_rows = sqlx::query_as::<_, RelationshipRow>(
                r#"
                SELECT
                    r.friendship_id AS request_id,
                    CASE
                        WHEN r.sender_id = $1 THEN r.receiver_id
                        ELSE r.sender_id
                    END AS user_id,
                    u.public_id AS user_public_id,
                    u.username,
                    COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                    COALESCE(u.profile_photo_style, '') AS profile_photo_style,
                    LOWER(COALESCE(r.status, '')) AS relationship_kind,
                    CASE
                        WHEN r.sender_id = $1 THEN 'outgoing'
                        ELSE 'incoming'
                    END AS direction
                FROM relationship r
                JOIN users u ON u.id = CASE
                    WHEN r.sender_id = $1 THEN r.receiver_id
                    ELSE r.sender_id
                END
                WHERE r.sender_id = $1 OR r.receiver_id = $1
                ORDER BY r.friendship_id DESC
                "#,
            )
            .bind(current_user_id)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

            let mut pending_requests = Vec::new();
            let mut incoming_requests = Vec::new();
            let mut outgoing_requests = Vec::new();
            let mut friendships = Vec::new();
            let mut blocked_users = Vec::new();

            for row in relationship_rows {
                match (row.relationship_kind.as_str(), row.direction.as_str()) {
                    ("friend", _) | ("friends", _) | ("friendship", _) | ("accepted", _) => {
                        friendships.push(RelationshipUser {
                            user_id: row.user_id,
                            public_id: row.user_public_id,
                            username: row.username,
                            profile_photo_url: row.profile_photo_url.clone(),
                            profile_photo_style: row.profile_photo_style.clone(),
                        })
                    }
                    ("pending", "incoming")
                    | ("request", "incoming")
                    | ("requested", "incoming")
                    | ("friend_request", "incoming") => {
                        pending_requests.push(PendingFriendRequest {
                            request_id: row.request_id,
                            sender_id: row.user_id,
                            receiver_id: current_user_id,
                        });
                        incoming_requests.push(IncomingFriendRequestView {
                            request_id: row.request_id,
                            sender_id: row.user_id,
                            sender_public_id: row.user_public_id,
                            sender_username: row.username,
                            sender_profile_photo_url: row.profile_photo_url.clone(),
                            sender_profile_photo_style: row.profile_photo_style.clone(),
                        });
                    }
                    ("pending", "outgoing")
                    | ("request", "outgoing")
                    | ("requested", "outgoing")
                    | ("friend_request", "outgoing") => {
                        outgoing_requests.push(OutgoingFriendRequestView {
                            request_id: row.request_id,
                            receiver_id: row.user_id,
                            receiver_public_id: row.user_public_id,
                            receiver_username: row.username,
                            receiver_profile_photo_url: row.profile_photo_url.clone(),
                            receiver_profile_photo_style: row.profile_photo_style.clone(),
                        });
                    }
                    ("blocked", _) | ("block", _) => blocked_users.push(RelationshipUser {
                        user_id: row.user_id,
                        public_id: row.user_public_id,
                        username: row.username,
                        profile_photo_url: row.profile_photo_url,
                        profile_photo_style: row.profile_photo_style,
                    }),
                    _ => {}
                }
            }

            (
                pending_requests,
                incoming_requests,
                outgoing_requests,
                friendships,
                blocked_users,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };

    let is_own_profile =
        profile_found && current_public_id != 0 && current_public_id == profile_public_id;

    let can_view_profile_birthday = is_own_profile || profile_birthday_public;
    let profile_birthday = if can_view_profile_birthday {
        profile_birthday
    } else {
        String::new()
    };
    let profile_first_name = if is_own_profile {
        profile_first_name
    } else {
        if profile_first_name_public {
            profile_first_name
        } else {
            String::new()
        }
    };
    let profile_last_name = if is_own_profile {
        profile_last_name
    } else {
        if profile_last_name_public {
            profile_last_name
        } else {
            String::new()
        }
    };
    let profile_display_name =
        format!("{} {}", profile_first_name.trim(), profile_last_name.trim())
            .trim()
            .to_string();
    let has_visible_profile_name = !profile_display_name.is_empty();

    let has_friendship_with_profile = friendships
        .iter()
        .any(|friend| friend.user_id == profile_id);
    let incoming_request_id_for_profile = incoming_requests
        .iter()
        .find(|req| req.sender_id == profile_id)
        .map(|req| req.request_id)
        .unwrap_or(0);
    let has_incoming_request_from_profile = incoming_request_id_for_profile != 0;
    let has_outgoing_request_to_profile = outgoing_requests
        .iter()
        .any(|req| req.receiver_id == profile_id);
    let has_following_profile = if let Some(current_user_id) = current_user_id {
        if profile_found && current_user_id != profile_id {
            sqlx::query_scalar::<_, bool>(
                r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM (
                        SELECT LOWER(COALESCE(r.status, '')) AS relationship_kind
                        FROM relationship r
                        WHERE r.sender_id = $1
                          AND r.receiver_id = $2
                        ORDER BY r.friendship_id DESC
                        LIMIT 1
                    ) latest
                    WHERE latest.relationship_kind IN ('follow', 'following', 'follower')
                )
                "#,
            )
            .bind(current_user_id)
            .bind(profile_id)
            .fetch_one(&pool)
            .await
            .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };
    let has_blocked_profile = blocked_users
        .iter()
        .any(|blocked| blocked.user_id == profile_id);
    let profile_posts = if profile_found {
        load_profile_posts(&pool, profile_id, current_user_id).await
    } else {
        Vec::new()
    };
    let profile_friends = if profile_found {
        load_profile_friends(&pool, profile_id).await
    } else {
        Vec::new()
    };
    let profile_uploaded_images = if profile_found {
        load_profile_uploaded_images(&pool, profile_id).await
    } else {
        Vec::new()
    };
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let template = UserTemplate {
        title,
        id: current_public_id,
        user_id: current_user_id.unwrap_or(0),
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_found,
        profile_id,
        profile_public_id,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        viewed_profile_photo_url,
        viewed_profile_photo_style,
        profile_background_photo_url,
        profile_background_photo_style,
        profile_username,
        profile_birthday,
        profile_display_name,
        has_visible_profile_name,
        can_view_profile_birthday,
        profile_created_at,
        profile_first_name,
        profile_last_name,
        profile_email,
        profile_bio_description,
        is_instavox_team_profile,
        is_remote_profile,
        remote_profile_url,
        pending_requests,
        incoming_requests,
        outgoing_requests,
        friendships,
        blocked_users,
        has_friendship_with_profile,
        has_incoming_request_from_profile,
        incoming_request_id_for_profile,
        has_outgoing_request_to_profile,
        has_following_profile,
        has_blocked_profile,
        is_own_profile,
        profile_posts,
        profile_friends,
        profile_uploaded_images,
        unread_notifications_count,
        notifications,
    };
    render_template_response(&template)
}


pub async fn load_user_identity(pool: &PgPool, user_id: i32) -> Option<(String, i64)> {
    sqlx::query_as::<_, (String, i64)>(
        r#"
        SELECT username, public_id
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}


pub async fn load_public_user_id(pool: &PgPool, user_id: i32) -> Option<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT public_id
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

pub async fn load_username_by_user_id(pool: &PgPool, user_id: i32) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT username
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

pub async fn load_settings_user(pool: &PgPool, user_id: i32) -> Option<SettingsUserRow> {
    sqlx::query_as::<_, SettingsUserRow>(
        r#"
        SELECT
            public_id,
            COALESCE(NULLIF(profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(profile_photo_style, '') AS profile_photo_style,
            COALESCE(NULLIF(background_photo_url, ''), '/public/pexels-enginakyurt-17902901.webp') AS background_photo_url,
            COALESCE(background_photo_style, '') AS background_photo_style,
            email,
            COALESCE(federation_enabled, TRUE) AS federation_enabled,
            COALESCE(NULLIF(federation_display_name_mode, ''), 'full_name') AS federation_display_name_mode,
            COALESCE(first_name_public, TRUE) AS first_name_public,
            COALESCE(last_name_public, TRUE) AS last_name_public,
            COALESCE(birthday_public, TRUE) AS birthday_public
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}


#[derive(Deserialize)]
pub struct SettingsQuery {
    pub password_status: Option<String>,
    pub email_status: Option<String>,
    pub federation_status: Option<String>,
    pub privacy_status: Option<String>,
    pub delete_status: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SettingsUserRow {
    public_id: i64,
    profile_photo_url: String,
    profile_photo_style: String,
    background_photo_url: String,
    background_photo_style: String,
    email: String,
    federation_enabled: bool,
    federation_display_name_mode: String,
    first_name_public: bool,
    last_name_public: bool,
    birthday_public: bool,
}

pub async fn settings(
    Query(query): Query<SettingsQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };
    let is_moderator = load_is_moderator(&pool, Some(current_user_id)).await;

    let Some(settings_user) = load_settings_user(&pool, current_user_id).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = session.insert("public_id", settings_user.public_id).await {
        tracing::warn!("failed to cache public_id in session: {}", err);
    }
    if let Err(err) = session
        .insert("profile_photo_url", &settings_user.profile_photo_url)
        .await
    {
        tracing::warn!("failed to cache profile_photo_url in session: {}", err);
    }
    if let Err(err) = session
        .insert("profile_photo_style", &settings_user.profile_photo_style)
        .await
    {
        tracing::warn!("failed to cache profile_photo_style in session: {}", err);
    }

    let uploaded_profile_images = list_uploaded_images(&pool, current_user_id, "profile").await;
    let uploaded_background_images =
        list_uploaded_images(&pool, current_user_id, "background").await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, Some(current_user_id)).await;
    let (password_message, password_success) =
        settings_password_feedback(query.password_status.as_deref());
    let (email_message, email_success) = settings_email_feedback(query.email_status.as_deref());
    let (federation_message, federation_success) =
        settings_federation_feedback(query.federation_status.as_deref());
    let (privacy_message, privacy_success) =
        settings_privacy_feedback(query.privacy_status.as_deref());
    let (delete_message, delete_success) = settings_delete_feedback(query.delete_status.as_deref());

    let template = SettingsTemplate {
        title: "Settings".to_string(),
        id: settings_user.public_id,
        user_id: current_user_id,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        current_profile_photo_url: settings_user.profile_photo_url,
        current_profile_photo_style: settings_user.profile_photo_style,
        current_background_photo_url: settings_user.background_photo_url,
        current_background_photo_style: settings_user.background_photo_style,
        uploaded_profile_images,
        uploaded_background_images,
        unread_notifications_count,
        notifications,
        username: session_string(&session, "username", "").await,
        password_message,
        password_success,
        email_message,
        email_success,
        current_email: settings_user.email,
        federation_enabled: settings_user.federation_enabled,
        federation_display_name_mode: settings_user.federation_display_name_mode,
        federation_message,
        federation_success,
        privacy_message,
        privacy_success,
        first_name_public: settings_user.first_name_public,
        last_name_public: settings_user.last_name_public,
        birthday_public: settings_user.birthday_public,
        delete_message,
        delete_success,
    };

    render_template_response(&template)
}


pub async fn load_profile_posts(
    pool: &PgPool,
    profile_user_id: i32,
    current_user_id: Option<i32>,
) -> Vec<IndexPostView> {
    load_profile_posts_segment(
        pool,
        profile_user_id,
        current_user_id,
        None,
        None,
        FEED_PAGE_MAX_LIMIT,
    )
    .await
    .posts
}

pub async fn load_profile_posts_segment(
    pool: &PgPool,
    profile_user_id: i32,
    current_user_id: Option<i32>,
    before_post_id: Option<i64>,
    after_post_id: Option<i64>,
    limit: i64,
) -> FeedPageResponse {
    let page_limit = normalize_feed_limit(Some(limit));
    let fetch_limit = page_limit + 1;
    let viewer_user_id = current_user_id.unwrap_or(0);
    let after_post_id = after_post_id.filter(|value| *value > 0);
    let before_post_id = before_post_id.filter(|value| *value > 0);

    let mut post_rows = if let Some(after_id) = after_post_id {
        sqlx::query_as::<_, FeedPostRow>(
            r#"
            SELECT
                p.post_id,
                u.public_id AS author_public_id,
                u.username AS author_username,
                COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS author_profile_photo_url,
                COALESCE(u.profile_photo_style, '') AS author_profile_photo_style,
                COALESCE(NULLIF(p.body, ''), '') AS body,
                COALESCE(NULLIF(p.link_url, ''), '') AS link_url,
                LOWER(COALESCE(NULLIF(p.visibility, ''), 'public')) AS visibility,
                COALESCE(NULLIF(c.name, ''), '') AS community_name,
                COALESCE(NULLIF(c.slug, ''), '') AS community_slug,
                TO_CHAR(p.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM posts p
            JOIN users u ON u.id = p.user_id
            LEFT JOIN community_page c ON c.community_id = p.community_id
            WHERE p.user_id = $1
              AND p.post_id > $2
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $4
              )
              AND (
                  p.community_id IS NULL
                  OR p.user_id = $4
                  OR (
                      $4 > 0
                      AND EXISTS (
                          SELECT 1
                          FROM community_member cm
                          WHERE cm.community_id = p.community_id
                            AND cm.user_id = $4
                      )
                  )
              )
            ORDER BY p.created_at ASC, p.post_id ASC
            LIMIT $3
            "#,
        )
        .bind(profile_user_id)
        .bind(after_id)
        .bind(fetch_limit)
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else if let Some(before_id) = before_post_id {
        sqlx::query_as::<_, FeedPostRow>(
            r#"
            SELECT
                p.post_id,
                u.public_id AS author_public_id,
                u.username AS author_username,
                COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS author_profile_photo_url,
                COALESCE(u.profile_photo_style, '') AS author_profile_photo_style,
                COALESCE(NULLIF(p.body, ''), '') AS body,
                COALESCE(NULLIF(p.link_url, ''), '') AS link_url,
                LOWER(COALESCE(NULLIF(p.visibility, ''), 'public')) AS visibility,
                COALESCE(NULLIF(c.name, ''), '') AS community_name,
                COALESCE(NULLIF(c.slug, ''), '') AS community_slug,
                TO_CHAR(p.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM posts p
            JOIN users u ON u.id = p.user_id
            LEFT JOIN community_page c ON c.community_id = p.community_id
            WHERE p.user_id = $1
              AND p.post_id < $2
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $4
              )
              AND (
                  p.community_id IS NULL
                  OR p.user_id = $4
                  OR (
                      $4 > 0
                      AND EXISTS (
                          SELECT 1
                          FROM community_member cm
                          WHERE cm.community_id = p.community_id
                            AND cm.user_id = $4
                      )
                  )
              )
            ORDER BY p.created_at DESC, p.post_id DESC
            LIMIT $3
            "#,
        )
        .bind(profile_user_id)
        .bind(before_id)
        .bind(fetch_limit)
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as::<_, FeedPostRow>(
            r#"
            SELECT
                p.post_id,
                u.public_id AS author_public_id,
                u.username AS author_username,
                COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS author_profile_photo_url,
                COALESCE(u.profile_photo_style, '') AS author_profile_photo_style,
                COALESCE(NULLIF(p.body, ''), '') AS body,
                COALESCE(NULLIF(p.link_url, ''), '') AS link_url,
                LOWER(COALESCE(NULLIF(p.visibility, ''), 'public')) AS visibility,
                COALESCE(NULLIF(c.name, ''), '') AS community_name,
                COALESCE(NULLIF(c.slug, ''), '') AS community_slug,
                TO_CHAR(p.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM posts p
            JOIN users u ON u.id = p.user_id
            LEFT JOIN community_page c ON c.community_id = p.community_id
            WHERE p.user_id = $1
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $3
              )
              AND (
                  p.community_id IS NULL
                  OR p.user_id = $3
                  OR (
                      $3 > 0
                      AND EXISTS (
                          SELECT 1
                          FROM community_member cm
                          WHERE cm.community_id = p.community_id
                            AND cm.user_id = $3
                      )
                  )
              )
            ORDER BY p.created_at DESC, p.post_id DESC
            LIMIT $2
            "#,
        )
        .bind(profile_user_id)
        .bind(fetch_limit)
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    };

    let has_more = (post_rows.len() as i64) > page_limit;
    if has_more {
        post_rows.pop();
    }

    let posts = build_feed_post_views(pool, post_rows, current_user_id).await;
    FeedPageResponse {
        next_before_post_id: posts.iter().map(|post| post.post_id).min(),
        next_after_post_id: posts.iter().map(|post| post.post_id).max(),
        posts,
        has_more,
    }
}



/*  -----------------------------------------------------
    |                                                   |
    | User Image section                                |
    |                                                   |
    -----------------------------------------------------
*/


async fn load_profile_friends(pool: &PgPool, profile_user_id: i32) -> Vec<ProfileFriendView> {
    let rows = sqlx::query_as::<_, ProfileFriendRow>(
        r#"
        SELECT
            CASE
                WHEN r.sender_id = $1 THEN r.receiver_id
                ELSE r.sender_id
            END AS user_id,
            u.public_id,
            u.username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS profile_photo_style
        FROM relationship r
        JOIN users u ON u.id = CASE
            WHEN r.sender_id = $1 THEN r.receiver_id
            ELSE r.sender_id
        END
        WHERE (r.sender_id = $1 OR r.receiver_id = $1)
          AND LOWER(COALESCE(r.status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
        ORDER BY COALESCE(r.modified_at, r.created_at) DESC, r.friendship_id DESC
        "#,
    )
    .bind(profile_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut seen_user_ids = BTreeSet::new();
    let mut friends = Vec::new();
    for row in rows {
        if !seen_user_ids.insert(row.user_id) {
            continue;
        }
        friends.push(ProfileFriendView {
            public_id: row.public_id,
            username: row.username,
            profile_photo_url: row.profile_photo_url,
            profile_photo_style: row.profile_photo_style,
        });
    }

    friends.sort_by(|a, b| {
        let a_lower = a.username.to_ascii_lowercase();
        let b_lower = b.username.to_ascii_lowercase();
        a_lower
            .cmp(&b_lower)
            .then_with(|| a.public_id.cmp(&b.public_id))
    });
    friends
}

async fn load_profile_uploaded_images(
    pool: &PgPool,
    profile_user_id: i32,
) -> Vec<UploadedUserImageView> {
    let rows = sqlx::query_as::<_, UploadedUserImageRow>(
        r#"
        SELECT
            upload_id,
            COALESCE(NULLIF(media_type, ''), 'image') AS media_type,
            file_url,
            COALESCE(created_at::TEXT, '') AS created_at
        FROM user_image_upload
        WHERE user_id = $1
        ORDER BY created_at DESC, upload_id DESC
        LIMIT 200
        "#,
    )
    .bind(profile_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| UploadedUserImageView {
            upload_id: row.upload_id,
            media_type: row.media_type,
            file_url: row.file_url,
            created_at: row.created_at,
        })
        .collect()
}


pub async fn settings_upload_profile_photo(
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let (file_name, content_type, bytes) = match read_uploaded_image(multipart).await {
        Ok(data) => data,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    let file_url = match save_settings_image_file(
        current_user_id,
        "profile",
        file_name.as_deref(),
        content_type.as_deref(),
        &bytes,
    )
    .await
    {
        Ok(url) => url,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    if let Err(err) =
        store_uploaded_image_record(&pool, current_user_id, "profile", &file_url).await
    {
        tracing::warn!("settings_upload_profile_photo record failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to save uploaded image",
        )
            .into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET profile_photo_url = $1,
            profile_photo_style = ''
        WHERE id = $2
        "#,
    )
    .bind(&file_url)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_upload_profile_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update profile photo",
        )
            .into_response();
    }

    if let Err(err) = session.insert("profile_photo_url", &file_url).await {
        tracing::warn!("failed to update profile_photo_url in session: {}", err);
    }
    if let Err(err) = session.insert("profile_photo_style", "").await {
        tracing::warn!("failed to clear profile_photo_style in session: {}", err);
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_upload_profile_photo")
        .await;

    Redirect::to("/settings").into_response()
}


#[derive(Deserialize)]
pub struct SelectUploadedImageForm {
    pub file_url: String,
    pub crop_apply: Option<String>,
    pub crop_x: Option<String>,
    pub crop_y: Option<String>,
    pub crop_zoom: Option<String>,
}

pub async fn settings_select_profile_photo(
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<SelectUploadedImageForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let selected_url = form.file_url.trim();
    if selected_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "No profile image selected").into_response();
    }

    if !is_owned_uploaded_image(&pool, current_user_id, "profile", selected_url).await {
        return (StatusCode::FORBIDDEN, "Selected image is not available").into_response();
    }
    let crop_style = crop_style_from_form(&form);

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET profile_photo_url = $1,
            profile_photo_style = $2
        WHERE id = $3
        "#,
    )
    .bind(selected_url)
    .bind(&crop_style)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_select_profile_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update profile photo",
        )
            .into_response();
    }

    if let Err(err) = session.insert("profile_photo_url", selected_url).await {
        tracing::warn!("failed to update profile_photo_url in session: {}", err);
    }
    if let Err(err) = session.insert("profile_photo_style", &crop_style).await {
        tracing::warn!("failed to update profile_photo_style in session: {}", err);
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_select_profile_photo")
        .await;

    Redirect::to("/settings").into_response()
}

pub async fn settings_reset_profile_photo(
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET profile_photo_url = $1,
            profile_photo_style = ''
        WHERE id = $2
        "#,
    )
    .bind(DEFAULT_PROFILE_PHOTO_URL)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_reset_profile_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset profile photo",
        )
            .into_response();
    }

    if let Err(err) = session
        .insert("profile_photo_url", DEFAULT_PROFILE_PHOTO_URL)
        .await
    {
        tracing::warn!("failed to update profile_photo_url in session: {}", err);
    }
    if let Err(err) = session.insert("profile_photo_style", "").await {
        tracing::warn!("failed to clear profile_photo_style in session: {}", err);
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_reset_profile_photo")
        .await;

    Redirect::to("/settings").into_response()
}


pub async fn delete_profile_uploaded_image(
    session: Session,
    State(pool): State<PgPool>,
    Path(upload_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let file_url = match sqlx::query_scalar::<_, String>(
        r#"
        SELECT file_url
        FROM user_image_upload
        WHERE upload_id = $1
          AND user_id = $2
        "#,
    )
    .bind(upload_id)
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("delete_profile_uploaded_image lookup failed: {}", err);
            None
        }
    };

    let Some(file_url) = file_url else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM user_image_upload
        WHERE upload_id = $1
          AND user_id = $2
        "#,
    )
    .bind(upload_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "delete_profile_uploaded_image record delete failed: {}",
            err
        );
    }

    let profile_reset = sqlx::query(
        r#"
        UPDATE users
        SET profile_photo_url = $1,
            profile_photo_style = ''
        WHERE id = $2
          AND profile_photo_url = $3
        "#,
    )
    .bind(DEFAULT_PROFILE_PHOTO_URL)
    .bind(current_user_id)
    .bind(&file_url)
    .execute(&pool)
    .await
    .map(|result| result.rows_affected() > 0)
    .unwrap_or_else(|err| {
        tracing::warn!(
            "delete_profile_uploaded_image profile reset failed: {}",
            err
        );
        false
    });

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET background_photo_url = $1,
            background_photo_style = ''
        WHERE id = $2
          AND background_photo_url = $3
        "#,
    )
    .bind(DEFAULT_BACKGROUND_PHOTO_URL)
    .bind(current_user_id)
    .bind(&file_url)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "delete_profile_uploaded_image background reset failed: {}",
            err
        );
    }

    if profile_reset {
        if let Err(err) = session
            .insert("profile_photo_url", DEFAULT_PROFILE_PHOTO_URL)
            .await
        {
            tracing::warn!(
                "delete_profile_uploaded_image session update failed: {}",
                err
            );
        }
        if let Err(err) = session.insert("profile_photo_style", "").await {
            tracing::warn!(
                "delete_profile_uploaded_image session style reset failed: {}",
                err
            );
        }
    }

    if let Some(path) = uploaded_user_image_disk_path(&file_url) {
        if let Err(err) = fs::remove_file(path).await {
            if err.kind() != ErrorKind::NotFound {
                tracing::warn!("delete_profile_uploaded_image file delete failed: {}", err);
            }
        }
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}


pub struct UploadedUserImageView {
    pub upload_id: i64,
    pub media_type: String,
    pub file_url: String,
    pub created_at: String,
}

pub async fn save_settings_image_file(
    user_id: i32,
    media_type: &str,
    file_name: Option<&str>,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("Uploaded file is empty".to_string());
    }
    if bytes.len() > MAX_SETTINGS_IMAGE_BYTES {
        return Err("Image must be 8MB or smaller before compression".to_string());
    }

    detect_image_extension(file_name, content_type)
        .ok_or_else(|| "Only image files are allowed".to_string())?;
    let compressed = compress_upload_to_jpeg(bytes.to_vec()).await?;
    let stem = normalize_uploaded_filename_stem(file_name);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let stored_name = format!("{}_{}.{}", timestamp, stem, COMPRESSED_IMAGE_EXTENSION);

    let mut dir = PathBuf::from("public/uploads/user-images");
    dir.push(user_id.to_string());
    dir.push(media_type);
    fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("Failed to prepare upload folder: {}", err))?;

    let file_path = dir.join(&stored_name);
    fs::write(&file_path, compressed)
        .await
        .map_err(|_| "Failed to save uploaded image".to_string())?;

    Ok(format!(
        "/public/uploads/user-images/{}/{}/{}",
        user_id, media_type, stored_name
    ))
}

pub async fn store_uploaded_image_record(
    pool: &PgPool,
    user_id: i32,
    media_type: &str,
    file_url: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO user_image_upload (user_id, media_type, file_url, created_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (user_id, media_type, file_url) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(media_type)
    .bind(file_url)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn is_owned_uploaded_image(
    pool: &PgPool,
    user_id: i32,
    media_type: &str,
    file_url: &str,
) -> bool {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM user_image_upload
            WHERE user_id = $1
              AND media_type = $2
              AND file_url = $3
        )
        "#,
    )
    .bind(user_id)
    .bind(media_type)
    .bind(file_url)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}



/*  -----------------------------------------------------
    |                                                   |
    | Banner section                                    |
    |                                                   |
    -----------------------------------------------------
*/

pub async fn settings_upload_background_photo(
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let (file_name, content_type, bytes) = match read_uploaded_image(multipart).await {
        Ok(data) => data,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    let file_url = match save_settings_image_file(
        current_user_id,
        "background",
        file_name.as_deref(),
        content_type.as_deref(),
        &bytes,
    )
    .await
    {
        Ok(url) => url,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    if let Err(err) =
        store_uploaded_image_record(&pool, current_user_id, "background", &file_url).await
    {
        tracing::warn!("settings_upload_background_photo record failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to save uploaded image",
        )
            .into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET background_photo_url = $1,
            background_photo_style = ''
        WHERE id = $2
        "#,
    )
    .bind(&file_url)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_upload_background_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update background photo",
        )
            .into_response();
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_upload_background_photo")
        .await;

    Redirect::to("/settings").into_response()
}


pub async fn settings_select_background_photo(
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<SelectUploadedImageForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let selected_url = form.file_url.trim();
    if selected_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "No background image selected").into_response();
    }

    if !is_owned_uploaded_image(&pool, current_user_id, "background", selected_url).await {
        return (StatusCode::FORBIDDEN, "Selected image is not available").into_response();
    }
    let crop_style = crop_style_from_form(&form);

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET background_photo_url = $1,
            background_photo_style = $2
        WHERE id = $3
        "#,
    )
    .bind(selected_url)
    .bind(&crop_style)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_select_background_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update background photo",
        )
            .into_response();
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_select_background_photo")
        .await;

    Redirect::to("/settings").into_response()
}


pub async fn settings_reset_background_photo(
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET background_photo_url = $1,
            background_photo_style = ''
        WHERE id = $2
        "#,
    )
    .bind(DEFAULT_BACKGROUND_PHOTO_URL)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_reset_background_photo update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset background photo",
        )
            .into_response();
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_reset_background_photo")
        .await;

    Redirect::to("/settings").into_response()
}



/*  -----------------------------------------------------
    |                                                   |
    | Federation Settings section                       |
    |                                                   |
    -----------------------------------------------------
*/




/*  -----------------------------------------------------
    |                                                   |
    | Local Settings section                            |
    |                                                   |
    -----------------------------------------------------
*/

// User Email
#[derive(Deserialize)]
pub struct ChangeEmailForm {
    pub current_password: String,
    pub new_email: String,
    pub confirm_email: String,
}

pub async fn settings_change_email(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<ChangeEmailForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if payload.current_password.is_empty()
        || payload.new_email.trim().is_empty()
        || payload.confirm_email.trim().is_empty()
    {
        return Redirect::to("/settings?email_status=missing").into_response();
    }

    let next_email = payload.new_email.trim().to_string();
    let confirm_email = payload.confirm_email.trim();
    if next_email != confirm_email {
        return Redirect::to("/settings?email_status=mismatch").into_response();
    }

    if !is_valid_signup_email(&next_email) {
        return Redirect::to("/settings?email_status=invalid").into_response();
    }

    let (stored_hash, current_email) = match sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT password, email
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return Redirect::to("/login").into_response(),
        Err(err) => {
            tracing::warn!("settings_change_email load user failed: {}", err);
            return Redirect::to("/settings?email_status=update_error").into_response();
        }
    };

    let is_current_valid = match verify_password(&payload.current_password, &stored_hash) {
        Ok(valid) => valid,
        Err(err) => {
            tracing::warn!("settings_change_email verify failed: {}", err);
            false
        }
    };

    if !is_current_valid {
        return Redirect::to("/settings?email_status=current_invalid").into_response();
    }

    if current_email.eq_ignore_ascii_case(&next_email) {
        return Redirect::to("/settings?email_status=same").into_response();
    }

    let email_taken = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE id <> $1
              AND LOWER(email) = LOWER($2)
        )
        "#,
    )
    .bind(current_user_id)
    .bind(&next_email)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if email_taken {
        return Redirect::to("/settings?email_status=taken").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET email = $1
        WHERE id = $2
        "#,
    )
    .bind(&next_email)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_change_email update failed: {}", err);
        return Redirect::to("/settings?email_status=update_error").into_response();
    }

    Redirect::to("/settings?email_status=changed").into_response()
}


// User Password
#[derive(Deserialize)]
pub struct ChangePasswordForm {
    pub current_password: String,
    pub new_password: String,
    pub confirm_password: String,
}

pub async fn settings_change_password(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Form(payload): Form<ChangePasswordForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if payload.current_password.is_empty()
        || payload.new_password.is_empty()
        || payload.confirm_password.is_empty()
    {
        return Redirect::to("/settings?password_status=missing").into_response();
    }

    if payload.new_password != payload.confirm_password {
        return Redirect::to("/settings?password_status=mismatch").into_response();
    }

    if validate_secure_password(&payload.new_password).is_err() {
        return Redirect::to("/settings?password_status=weak").into_response();
    }

    if payload.new_password == payload.current_password {
        return Redirect::to("/settings?password_status=same").into_response();
    }

    let stored_hash = match sqlx::query_scalar::<_, String>(
        r#"
        SELECT password
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(hash)) => hash,
        Ok(None) => return Redirect::to("/login").into_response(),
        Err(err) => {
            tracing::warn!("settings_change_password load hash failed: {}", err);
            return Redirect::to("/settings?password_status=hash_error").into_response();
        }
    };

    let is_current_valid = match verify_password(&payload.current_password, &stored_hash) {
        Ok(valid) => valid,
        Err(err) => {
            tracing::warn!("settings_change_password verify failed: {}", err);
            false
        }
    };

    if !is_current_valid {
        return Redirect::to("/settings?password_status=current_invalid").into_response();
    }

    let next_hash = match hash_password(&payload.new_password) {
        Ok(hash) => hash,
        Err(err) => {
            tracing::warn!("settings_change_password hash failed: {}", err);
            return Redirect::to("/settings?password_status=hash_error").into_response();
        }
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET password = $1
        WHERE id = $2
        "#,
    )
    .bind(next_hash)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_change_password update failed: {}", err);
        return Redirect::to("/settings?password_status=hash_error").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM remember_session
        WHERE user_id = $1
        "#,
    )
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_change_password remember cleanup failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    let redirect_with_status = if redirect_to.starts_with("/settings") {
        if redirect_to.contains('?') {
            format!("{}&password_status=changed", redirect_to)
        } else {
            format!("{}?password_status=changed", redirect_to)
        }
    } else {
        "/settings?password_status=changed".to_string()
    };

    Redirect::to(&redirect_with_status).into_response()
}


pub async fn list_uploaded_images(pool: &PgPool, user_id: i32, media_type: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT file_url
        FROM user_image_upload
        WHERE user_id = $1
          AND media_type = $2
        ORDER BY created_at DESC, upload_id DESC
        "#,
    )
    .bind(user_id)
    .bind(media_type)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

fn uploaded_user_image_disk_path(file_url: &str) -> Option<PathBuf> {
    let suffix = file_url.strip_prefix("/public/uploads/user-images/")?;
    if suffix.trim().is_empty() {
        return None;
    }

    let relative = FsPath::new(suffix);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    Some(PathBuf::from("public/uploads/user-images").join(relative))
}


// User Bio
#[derive(Deserialize)]
pub struct UpdateProfileBioForm {
    pub bio_description: String,
}

pub async fn update_profile_bio(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Form(payload): Form<UpdateProfileBioForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let mut bio_description = payload.bio_description.replace("\r\n", "\n");
    if bio_description.chars().count() > MAX_PROFILE_BIO_LENGTH {
        bio_description = bio_description
            .chars()
            .take(MAX_PROFILE_BIO_LENGTH)
            .collect();
    }

    let profile_bio_updated = match sqlx::query(
        r#"
        UPDATE users
        SET bio_description = $1
        WHERE id = $2
        "#,
    )
    .bind(bio_description)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!("update_profile_bio failed: {}", err);
            false
        }
    };

    if profile_bio_updated {
        notify_profile_update_to_federation(&pool, current_user_id, "update_profile_bio").await;
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}



// User Privacy
#[derive(Deserialize)]
pub struct UpdateProfilePrivacyForm {
    pub first_name_privacy: String,
    pub last_name_privacy: String,
    pub birthday_privacy: String,
}

fn parse_profile_field_privacy_setting(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "public" | "visible" | "show" | "shown" | "enabled" | "true" | "1" => Some(true),
        "private" | "hidden" | "hide" | "disabled" | "false" | "0" => Some(false),
        _ => None,
    }
}

pub async fn settings_update_profile_privacy(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<UpdateProfilePrivacyForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(first_name_public) = parse_profile_field_privacy_setting(&payload.first_name_privacy)
    else {
        return Redirect::to("/settings?privacy_status=invalid").into_response();
    };
    let Some(last_name_public) = parse_profile_field_privacy_setting(&payload.last_name_privacy)
    else {
        return Redirect::to("/settings?privacy_status=invalid").into_response();
    };
    let Some(birthday_public) = parse_profile_field_privacy_setting(&payload.birthday_privacy)
    else {
        return Redirect::to("/settings?privacy_status=invalid").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET first_name_public = $1,
            last_name_public = $2,
            birthday_public = $3
        WHERE id = $4
        "#,
    )
    .bind(first_name_public)
    .bind(last_name_public)
    .bind(birthday_public)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_update_profile_privacy update failed: {}", err);
        return Redirect::to("/settings?privacy_status=update_error").into_response();
    }

    notify_profile_update_to_federation(&pool, current_user_id, "settings_update_profile_privacy")
        .await;

    Redirect::to("/settings?privacy_status=changed").into_response()
}

fn settings_privacy_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "changed" => ("Profile privacy updated successfully.".to_string(), true),
        "invalid" => ("Invalid privacy setting value.".to_string(), false),
        "update_error" => (
            "Unable to update profile privacy right now.".to_string(),
            false,
        ),
        _ => (String::new(), false),
    }
}


// Delete User
#[derive(Deserialize)]
pub struct DeleteProfileForm {
    pub current_password: String,
}

pub async fn settings_delete_profile(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<DeleteProfileForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if payload.current_password.is_empty() {
        return Redirect::to("/settings?delete_status=missing").into_response();
    }

    let stored_hash = match sqlx::query_scalar::<_, String>(
        r#"
        SELECT password
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(hash)) => hash,
        Ok(None) => return Redirect::to("/login").into_response(),
        Err(err) => {
            tracing::warn!("settings_delete_profile load hash failed: {}", err);
            return Redirect::to("/settings?delete_status=failed").into_response();
        }
    };

    let is_current_valid = match verify_password(&payload.current_password, &stored_hash) {
        Ok(valid) => valid,
        Err(err) => {
            tracing::warn!("settings_delete_profile verify failed: {}", err);
            false
        }
    };

    if !is_current_valid {
        return Redirect::to("/settings?delete_status=current_invalid").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM remember_session
        WHERE user_id = $1
        "#,
    )
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_delete_profile remember cleanup failed: {}", err);
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM users
        WHERE id = $1
        "#,
    )
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_delete_profile user delete failed: {}", err);
        return Redirect::to("/settings?delete_status=failed").into_response();
    }

    let mut user_image_dir = PathBuf::from("public/uploads/user-images");
    user_image_dir.push(current_user_id.to_string());
    if let Err(err) = fs::remove_dir_all(&user_image_dir).await {
        if err.kind() != ErrorKind::NotFound {
            tracing::warn!("settings_delete_profile user-image cleanup failed: {}", err);
        }
    }

    let mut post_image_dir = PathBuf::from("public/uploads/posts");
    post_image_dir.push(current_user_id.to_string());
    if let Err(err) = fs::remove_dir_all(&post_image_dir).await {
        if err.kind() != ErrorKind::NotFound {
            tracing::warn!("settings_delete_profile post-image cleanup failed: {}", err);
        }
    }

    if let Err(err) = session.flush().await {
        tracing::warn!("settings_delete_profile session flush failed: {}", err);
    }

    Redirect::to("/login").into_response()
}

fn settings_delete_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "current_invalid" => ("Current password is incorrect.".to_string(), false),
        "missing" => (
            "Current password is required to delete profile.".to_string(),
            false,
        ),
        "failed" => ("Unable to delete profile right now.".to_string(), false),
        _ => (String::new(), false),
    }
}
