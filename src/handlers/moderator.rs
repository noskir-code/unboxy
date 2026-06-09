use std::collections::{BTreeMap, HashMap};

use askama::Template;
use axum::{Form, Json};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Response, Redirect, IntoResponse};
use tower_sessions::Session;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{CANONICAL_INSTAVOX_BASE_URL, CANONICAL_INSTAVOX_DOMAIN};
use crate::handlers::searching::{FeedPageQuery, is_fetch_request, load_community_by_slug};
use crate::handlers::session::{apply_session_identity, is_acting_as_team_session, load_session_identity_row, session_i32_with_default, session_public_user_id, session_string, session_user_id};
use crate::handlers::notifications::{HeaderNotificationRow, HeaderNotificationView, NOTIFICATION_PAGE_DEFAULT_LIMIT, NOTIFICATION_PAGE_MAX_LIMIT, NotificationPageItem, NotificationPageQuery, NotificationPageResponse, create_comment_like_notification, create_notification, load_header_notifications};
use crate::handlers::post::{IndexPostView, MAX_POST_COMMENT_LENGTH, MAX_POST_TEXT_LENGTH, POST_VISIBILITY_PUBLIC, PostOwnerRow, build_feed_post_views, can_view_post_for_user, extract_first_link_from_text, insert_post_from_draft, load_index_posts_segment, load_post_visibility_state, normalize_feed_limit, normalize_post_visibility, read_post_draft_from_multipart, resolve_first_image_from_link};
use crate::handlers::user::{ensure_moderator_team_user, load_profile_posts_segment, load_user_identity, redirect_back_path};
use crate::routes::render_template_response;
use crate::security::auth::{is_valid_signup_email, send_smtp_test_email};
use crate::handlers::post::{PostCommentPostRow, FeedPostRow};

const MODERATOR_USERS_LIMIT: i64 = 120;
const MODERATOR_POSTS_PAGE_SIZE: i64 = 20;
const MODERATOR_BETA_INVITES_LIMIT: i64 = 40;
const MODERATOR_BETA_SIGNUP_REQUESTS_LIMIT: i64 = 60;
const MODERATOR_REDACTED_POST_TEXT: &str = "Deleted by moderator";

const MODERATOR_TEAM_USERNAME: &str = "instavox-team";
const MODERATOR_TEAM_PREFERRED_USERNAME: &str = "instavoxteam";
const MODERATOR_TEAM_EMAIL: &str = "instavox-team@local.invalid";
pub const MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY: &str = "moderator_original_user_id";

#[derive(Template)]
#[template(path = "main/moderator.html")]
#[allow(dead_code)]
pub struct ModeratorTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    pub stats: ModeratorStatsView,
    pub users: Vec<ModeratorUserView>,
    pub posts: Vec<IndexPostView>,
    pub post_tab: String,
    pub is_open_post_tab: bool,
    pub is_verified_post_tab: bool,
    pub post_page: i64,
    pub has_prev_post_page: bool,
    pub has_next_post_page: bool,
    pub prev_post_page: i64,
    pub next_post_page: i64,
    pub reports: Vec<ModeratorReportView>,
    pub instances: Vec<ModeratorInstanceView>,
    pub remote_users: Vec<ModeratorRemoteUserView>,
    pub smtp_test_message: String,
    pub smtp_test_success: bool,
    pub team_post_message: String,
    pub team_post_success: bool,
    pub team_switch_message: String,
    pub team_switch_success: bool,
    pub instance_message: String,
    pub instance_success: bool,
    pub remote_user_message: String,
    pub remote_user_success: bool,
    pub is_acting_as_team: bool,
}


async fn ensure_moderator_session(pool: &PgPool, session: &Session) -> Result<i32, Response> {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Err(Redirect::to("/login").into_response());
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return Err(StatusCode::NOT_FOUND.into_response());
    }
    Ok(current_user_id)
}


#[derive(Deserialize)]
pub struct ModeratorDashboardQuery {
    pub post_page: Option<i64>,
    pub post_tab: Option<String>,
    pub smtp_status: Option<String>,
    pub beta_status: Option<String>,
    pub beta_request_status: Option<String>,
    pub team_post_status: Option<String>,
    pub team_switch_status: Option<String>,
    pub instance_status: Option<String>,
    pub remote_user_status: Option<String>,
}

pub async fn moderator_dashboard(
    Query(query): Query<ModeratorDashboardQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let session_username = session_string(&session, "username", "").await;
    let is_acting_as_team = is_acting_as_team_session(&session).await;

    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, Some(current_user_id)).await;
    let stats = load_moderator_stats(&pool).await;
    let users = load_moderator_users(&pool).await;
    let instances = load_moderator_instances(&pool).await;
    let remote_users = load_moderator_remote_users(&pool).await;
    let post_tab = normalize_moderator_post_tab(query.post_tab.as_deref()).to_string();
    let is_open_post_tab = post_tab == "open";
    let is_verified_post_tab = post_tab == "verified";
    let report_status = if is_verified_post_tab {
        "verified"
    } else {
        "open"
    };
    let post_page = query.post_page.unwrap_or(1).max(1);
    let (posts, has_next_post_page) =
        load_moderator_posts_page(&pool, post_page, report_status).await;
    let has_prev_post_page = post_page > 1;
    let prev_post_page = if has_prev_post_page { post_page - 1 } else { 1 };
    let next_post_page = post_page + 1;
    let reports = load_moderator_reports(&pool).await;
    let (smtp_test_message, smtp_test_success) = match query.smtp_status.as_deref() {
        Some("sent") => ("SMTP test email sent successfully.".to_string(), true),
        Some("invalid_email") => ("Please enter a valid email address.".to_string(), false),
        Some("config_error") => (
            "SMTP is not configured correctly. Check SMTP settings.".to_string(),
            false,
        ),
        Some("send_error") => (
            "Unable to send SMTP test email right now. Check server logs.".to_string(),
            false,
        ),
        _ => ("".to_string(), false),
    };
    let (team_post_message, team_post_success) = match query.team_post_status.as_deref() {
        Some("published") => ("Instavox Team post published.".to_string(), true),
        Some("invalid") => (
            "A post requires text or at least one image.".to_string(),
            false,
        ),
        Some("payload_error") => (
            "Unable to read the moderator post payload.".to_string(),
            false,
        ),
        Some("save_error") => (
            "Unable to publish the Instavox Team post right now.".to_string(),
            false,
        ),
        _ => ("".to_string(), false),
    };
    let (team_switch_message, team_switch_success) = match query.team_switch_status.as_deref() {
        Some("team") => ("You are now acting as Team Instavox.".to_string(), true),
        Some("moderator") => (
            "You are now back on your moderator account.".to_string(),
            true,
        ),
        Some("forbidden") => (
            "Only moderators can use Team Instavox switching.".to_string(),
            false,
        ),
        Some("save_error") => ("Unable to switch account right now.".to_string(), false),
        _ => ("".to_string(), false),
    };
    let (instance_message, instance_success) = match query.instance_status.as_deref() {
        Some("discovered") => ("Instance status set to discovered.".to_string(), true),
        Some("limited") => ("Instance status set to limited.".to_string(), true),
        Some("ban") => ("Instance banned.".to_string(), true),
        Some("invalid_host") => ("Invalid instance host.".to_string(), false),
        Some("save_error") => (
            "Unable to update instance status right now.".to_string(),
            false,
        ),
        _ => ("".to_string(), false),
    };
    let (remote_user_message, remote_user_success) = match query.remote_user_status.as_deref() {
        Some("discovered") => ("Remote user status set to discovered.".to_string(), true),
        Some("limited") => ("Remote user status set to limited.".to_string(), true),
        Some("ban") => ("Remote user banned.".to_string(), true),
        Some("invalid_actor") => ("Invalid remote user.".to_string(), false),
        Some("save_error") => (
            "Unable to update remote user status right now.".to_string(),
            false,
        ),
        _ => ("".to_string(), false),
    };

    let template = ModeratorTemplate {
        title: "Moderator Dashboard".to_string(),
        id: current_public_id,
        user_id: current_user_id,
        is_moderator: true,
        local_profile_domain: local_profile_domain(),
        username: session_username,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        stats,
        users,
        posts,
        post_tab,
        is_open_post_tab,
        is_verified_post_tab,
        post_page,
        has_prev_post_page,
        has_next_post_page,
        prev_post_page,
        next_post_page,
        reports,
        instances,
        remote_users,
        smtp_test_message,
        smtp_test_success,
        team_post_message,
        team_post_success,
        team_switch_message,
        team_switch_success,
        instance_message,
        instance_success,
        remote_user_message,
        remote_user_success,
        is_acting_as_team,
    };

    render_template_response(&template)
}


pub async fn moderator_switch_to_team(
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return Redirect::to("/moderator?team_switch_status=forbidden").into_response();
    }

    let team_user_id = match ensure_moderator_team_user(&pool).await {
        Ok(user_id) => user_id,
        Err(err) => {
            tracing::warn!("moderator_switch_to_team team user failed: {}", err);
            return Redirect::to("/moderator?team_switch_status=save_error").into_response();
        }
    };

    if current_user_id != team_user_id {
        let existing_original_id =
            session_i32_with_default(&session, MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY, 0).await;
        let original_id_to_keep = if existing_original_id > 0 {
            existing_original_id
        } else {
            current_user_id
        };
        if let Err(err) = session
            .insert(
                MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY,
                original_id_to_keep,
            )
            .await
        {
            tracing::warn!(
                "moderator_switch_to_team failed to store original moderator id: {}",
                err
            );
            return Redirect::to("/moderator?team_switch_status=save_error").into_response();
        }
    }

    let Some(team_identity) = load_session_identity_row(&pool, team_user_id).await else {
        return Redirect::to("/moderator?team_switch_status=save_error").into_response();
    };

    if let Err(err) = apply_session_identity(&session, &team_identity).await {
        tracing::warn!("moderator_switch_to_team failed to apply identity: {}", err);
        return Redirect::to("/moderator?team_switch_status=save_error").into_response();
    }

    Redirect::to("/moderator?team_switch_status=team").into_response()
}

pub async fn moderator_switch_back_from_team(
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return Redirect::to("/moderator?team_switch_status=forbidden").into_response();
    }

    let original_moderator_id =
        session_i32_with_default(&session, MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY, 0).await;
    if original_moderator_id <= 0 {
        return Redirect::to("/moderator?team_switch_status=moderator").into_response();
    }

    let original_role = load_user_role(&pool, original_moderator_id).await;
    if !is_moderator_role(&original_role) {
        let _ = session
            .insert(MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY, 0_i32)
            .await;
        return Redirect::to("/moderator?team_switch_status=forbidden").into_response();
    }

    let Some(original_identity) = load_session_identity_row(&pool, original_moderator_id).await
    else {
        return Redirect::to("/moderator?team_switch_status=save_error").into_response();
    };

    if let Err(err) = apply_session_identity(&session, &original_identity).await {
        tracing::warn!(
            "moderator_switch_back_from_team failed to apply identity: {}",
            err
        );
        return Redirect::to("/moderator?team_switch_status=save_error").into_response();
    }

    if let Err(err) = session
        .insert(MODERATOR_ACTING_ORIGINAL_ID_SESSION_KEY, 0_i32)
        .await
    {
        tracing::warn!(
            "moderator_switch_back_from_team failed to clear original moderator id: {}",
            err
        );
    }

    Redirect::to("/moderator?team_switch_status=moderator").into_response()
}


fn normalize_moderation_role(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "user" => Some("user"),
        "mod" | "moderator" => Some("mod"),
        "suspended" | "suspend" => Some("suspended"),
        "ban" | "banned" => Some("ban"),
        _ => None,
    }
}

fn is_moderator_role(role: &str) -> bool {
    matches!(role, "mod" | "moderator")
}

async fn load_is_moderator(pool: &PgPool, user_id: Option<i32>) -> bool {
    match user_id {
        Some(user_id) => is_moderator_role(&load_user_role(pool, user_id).await),
        None => false,
    }
}



/*  -----------------------------------------------------
    |                                                   |
    | Moderation section                                |
    |                                                   |
    -----------------------------------------------------
*/

async fn load_moderator_stats(pool: &PgPool) -> ModeratorStatsView {
    let row = sqlx::query_as::<_, ModeratorStatsRow>(
        r#"
        SELECT
            COALESCE((SELECT COUNT(*)::BIGINT FROM users), 0) AS total_users,
            COALESCE((
                SELECT COUNT(*)::BIGINT
                FROM users
                WHERE LOWER(COALESCE(role::TEXT, 'user')) IN ('mod', 'moderator')
            ), 0) AS total_moderators,
            COALESCE((
                SELECT COUNT(*)::BIGINT
                FROM users
                WHERE LOWER(COALESCE(role::TEXT, 'user')) IN ('suspended', 'ban', 'banned')
            ), 0) AS total_suspended,
            COALESCE((SELECT COUNT(*)::BIGINT FROM posts), 0) AS total_posts
        "#,
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(ModeratorStatsRow {
        total_users: 0,
        total_moderators: 0,
        total_suspended: 0,
        total_posts: 0,
    });

    let instance_rows = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT
            COALESCE(NULLIF(protocol, ''), 'Other ActivityPub') AS protocol,
            LOWER(COALESCE(NULLIF(status, ''), 'discovered')) AS status
        FROM discovered_instance
        ORDER BY host ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut total_instances = 0_i64;
    let mut total_discovered_instances = 0_i64;
    let mut total_limited_instances = 0_i64;
    let mut total_banned_instances = 0_i64;
    let mut protocol_counts = BTreeMap::<String, i64>::new();
    for (protocol, status) in instance_rows {
        total_instances += 1;
        match normalize_discovery_status(&status).as_str() {
            "limited" => total_limited_instances += 1,
            "ban" => total_banned_instances += 1,
            _ => total_discovered_instances += 1,
        }
        *protocol_counts.entry(protocol).or_insert(0) += 1;
    }

    let remote_user_rows = sqlx::query_as::<_, (String,)>(
        r#"
        SELECT LOWER(COALESCE(NULLIF(status, ''), 'discovered')) AS status
        FROM ap_remote_actor
        ORDER BY actor_id ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut total_discovered_remote_users = 0_i64;
    let mut total_limited_remote_users = 0_i64;
    let mut total_banned_remote_users = 0_i64;
    for (status,) in remote_user_rows {
        match normalize_discovery_status(&status).as_str() {
            "limited" => total_limited_remote_users += 1,
            "ban" => total_banned_remote_users += 1,
            _ => total_discovered_remote_users += 1,
        }
    }

    let mut protocol_breakdown = protocol_counts
        .into_iter()
        .map(|(name, count)| ProtocolCountView { name, count })
        .collect::<Vec<_>>();
    protocol_breakdown.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

    ModeratorStatsView {
        total_users: row.total_users,
        total_moderators: row.total_moderators,
        total_suspended: row.total_suspended,
        total_posts: row.total_posts,
        total_federated_instances: total_instances,
        total_discovered_instances,
        total_limited_instances,
        total_banned_instances,
        total_discovered_remote_users,
        total_limited_remote_users,
        total_banned_remote_users,
        protocol_breakdown,
    }
}


#[derive(Deserialize)]
pub struct ModeratorSmtpTestForm {
    pub email: Option<String>,
}

pub async fn moderator_smtp_test(
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<ModeratorSmtpTestForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let target_email = if let Some(value) = form
        .email
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        value.to_string()
    } else {
        sqlx::query_scalar::<_, String>(
            r#"
            SELECT COALESCE(NULLIF(email, ''), '')
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(current_user_id)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
    };

    if !is_valid_signup_email(&target_email) {
        return Redirect::to("/moderator?smtp_status=invalid_email").into_response();
    }

    match send_smtp_test_email(&target_email).await {
        Ok(()) => Redirect::to("/moderator?smtp_status=sent").into_response(),
        Err(err) => {
            tracing::warn!("moderator_smtp_test failed for {}: {}", target_email, err);
            let normalized = err.to_ascii_lowercase();
            if normalized.contains("smtp is not configured")
                || normalized.contains("credentials are incomplete")
                || normalized.contains("unsupported smtp_security")
            {
                Redirect::to("/moderator?smtp_status=config_error").into_response()
            } else {
                Redirect::to("/moderator?smtp_status=send_error").into_response()
            }
        }
    }
}


pub async fn ban_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(_current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if !is_acting_as_team_session(&session).await {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let ban_result = sqlx::query(
        r#"
        UPDATE community_page
        SET status = 'banned', updated_at = CURRENT_TIMESTAMP
        WHERE community_id = $1
        "#,
    )
    .bind(existing.community_id)
    .execute(&pool)
    .await;

    match ban_result {
        Ok(result) if result.rows_affected() > 0 => {
            Redirect::to("/communities?create_status=banned").into_response()
        }
        _ => Redirect::to("/communities?create_status=moderation_failed").into_response(),
    }
}



/*  -----------------------------------------------------
    |                                                   |
    | Post section                                      |
    |                                                   |
    -----------------------------------------------------
*/
fn normalize_moderator_post_tab(raw: Option<&str>) -> &'static str {
    match raw.map(str::trim).map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "verified" => "verified",
        _ => "open",
    }
}

async fn load_moderator_posts_page(
    pool: &PgPool,
    page: i64,
    report_status: &str,
) -> (Vec<IndexPostView>, bool) {
    let safe_page = page.max(1);
    let offset = (safe_page - 1) * MODERATOR_POSTS_PAGE_SIZE;
    let rows = sqlx::query_as::<_, FeedPostRow>(
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
            COALESCE(TO_CHAR(p.created_at, 'YYYY-MM-DD HH24:MI'), '') AS created_at
        FROM posts p
        JOIN users u ON u.id = p.user_id
        LEFT JOIN community_page c ON c.community_id = p.community_id
        LEFT JOIN moderator_post_status mps ON mps.post_id = p.post_id
        WHERE LOWER(COALESCE(mps.status, 'open')) = $1
        ORDER BY p.post_id DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(report_status)
    .bind(MODERATOR_POSTS_PAGE_SIZE + 1)
    .bind(offset)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let has_next_page = rows.len() as i64 > MODERATOR_POSTS_PAGE_SIZE;
    let page_rows = if has_next_page {
        rows.into_iter()
            .take(MODERATOR_POSTS_PAGE_SIZE as usize)
            .collect()
    } else {
        rows
    };

    (
        build_feed_post_views(pool, page_rows, None).await,
        has_next_page,
    )
}


pub async fn moderator_mark_post_seen(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let fetch_request = is_fetch_request(&headers);

    let Some(current_user_id) = session_user_id(&session).await else {
        if fetch_request {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO moderator_post_status (post_id, status, verified_at, modified_at)
        VALUES ($1, 'verified', NOW(), NOW())
        ON CONFLICT (post_id) DO UPDATE
        SET status = 'verified',
            verified_at = NOW(),
            modified_at = NOW()
        "#,
    )
    .bind(post_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_mark_post_seen status update failed: {}", err);
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE app_report
        SET status = 'verified',
            modified_at = NOW()
        WHERE target_post_id = $1
          AND LOWER(COALESCE(kind, '')) = 'post'
          AND LOWER(COALESCE(status, 'open')) = 'open'
        "#,
    )
    .bind(post_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "moderator_mark_post_seen report verification update failed: {}",
            err
        );
    }

    if fetch_request {
        return Json(serde_json::json!({
            "success": true,
            "post_id": post_id
        }))
        .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}


pub async fn moderator_create_team_post(
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let team_user_id = match ensure_moderator_team_user(&pool).await {
        Ok(user_id) => user_id,
        Err(err) => {
            tracing::warn!("moderator_create_team_post team user failed: {}", err);
            return Redirect::to("/moderator?team_post_status=save_error").into_response();
        }
    };

    let draft =
        match read_post_draft_from_multipart(multipart, team_user_id, Some(POST_VISIBILITY_PUBLIC))
            .await
        {
            Ok(draft) => draft,
            Err((StatusCode::BAD_REQUEST, message))
                if message == "A post requires text or at least one image" =>
            {
                return Redirect::to("/moderator?team_post_status=invalid").into_response();
            }
            Err((StatusCode::BAD_REQUEST, _)) => {
                return Redirect::to("/moderator?team_post_status=payload_error").into_response();
            }
            Err(_) => {
                return Redirect::to("/moderator?team_post_status=save_error").into_response();
            }
        };

    match insert_post_from_draft(&pool, team_user_id, None, draft).await {
        Ok(post_id) => {
            let federation_pool = pool.clone();
            tokio::spawn(async move {
                if let Err(err) = crate::federation::send_post_to_subscribed_remote_inboxes(
                    &federation_pool,
                    team_user_id,
                    post_id,
                )
                .await
                {
                    tracing::warn!(
                        "team post federation delivery failed (post_id={}, user_id={}): {}",
                        post_id,
                        team_user_id,
                        err
                    );
                }
            });
            Redirect::to("/moderator?team_post_status=published").into_response()
        }
        Err(err) => {
            tracing::warn!("moderator_create_team_post publish failed: {}", err);
            Redirect::to("/moderator?team_post_status=save_error").into_response()
        }
    }
}

pub async fn moderator_delete_post(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM posts
        WHERE post_id = $1
        "#,
    )
    .bind(post_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_delete_post failed: {}", err);
    }

    Redirect::to("/moderator").into_response()
}

pub async fn moderator_redact_post(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE posts
        SET body = $1,
            link_url = '',
            updated_at = NOW()
        WHERE post_id = $2
        "#,
    )
    .bind(MODERATOR_REDACTED_POST_TEXT)
    .bind(post_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_redact_post update failed: {}", err);
        return Redirect::to("/moderator").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM post_image
        WHERE post_id = $1
        "#,
    )
    .bind(post_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_redact_post image cleanup failed: {}", err);
    }

    Redirect::to("/moderator").into_response()
}

pub async fn moderator_unredact_post(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE posts
        SET body = '',
            updated_at = NOW()
        WHERE post_id = $1
          AND BTRIM(COALESCE(body, '')) = $2
        "#,
    )
    .bind(post_id)
    .bind(MODERATOR_REDACTED_POST_TEXT)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_unredact_post failed: {}", err);
    }

    Redirect::to("/moderator").into_response()
}



/*  -----------------------------------------------------
    |                                                   |
    | Users section                                     |
    |                                                   |
    -----------------------------------------------------
*/

async fn load_user_role(pool: &PgPool, user_id: i32) -> String {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT LOWER(COALESCE(role::TEXT, 'user'))
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "user".to_string())
}

#[derive(sqlx::FromRow)]
struct ModeratorUserRow {
    public_id: i64,
    username: String,
    preferred_username: String,
    email: String,
    profile_photo_url: String,
    profile_photo_style: String,
    role: String,
    created_at: String,
}

fn public_base_url() -> String {
    CANONICAL_INSTAVOX_BASE_URL.to_string()
}

fn local_profile_domain_matches(candidate: &str) -> bool {
    let expected = local_profile_domain().to_ascii_lowercase();
    let candidate = candidate.trim().to_ascii_lowercase();
    if candidate == expected {
        return true;
    }

    let expected_without_port = expected.split(':').next().unwrap_or(expected.as_str());
    let candidate_without_port = candidate.split(':').next().unwrap_or(candidate.as_str());
    candidate_without_port == expected_without_port
}

fn local_user_profile_path(username: &str) -> String {
    format!("/user/{}@{}", username.trim(), local_profile_domain())
}


async fn load_moderator_users(pool: &PgPool) -> Vec<ModeratorUserView> {
    let rows = sqlx::query_as::<_, ModeratorUserRow>(
        r#"
        SELECT
            u.public_id,
            u.username,
            COALESCE(NULLIF(u.preferred_username, ''), '') AS preferred_username,
            COALESCE(NULLIF(u.email, ''), '') AS email,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS profile_photo_style,
            LOWER(COALESCE(u.role::TEXT, 'user')) AS role,
            COALESCE(TO_CHAR(u.created_at, 'YYYY-MM-DD HH24:MI'), '') AS created_at
        FROM users u
        ORDER BY u.id DESC
        LIMIT $1
        "#,
    )
    .bind(MODERATOR_USERS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| ModeratorUserView {
            public_id: row.public_id,
            username: row.username,
            preferred_username: row.preferred_username,
            email: row.email,
            profile_photo_url: row.profile_photo_url,
            profile_photo_style: row.profile_photo_style,
            role: row.role,
            created_at: row.created_at,
        })
        .collect()
}


async fn load_moderator_remote_users(pool: &PgPool) -> Vec<ModeratorRemoteUserView> {
    let rows = sqlx::query_as::<_, ModeratorRemoteUserRow>(
        r#"
        SELECT
            ra.actor_id,
            COALESCE(NULLIF(ra.host, ''), '') AS host,
            COALESCE(NULLIF(ra.preferred_username, ''), '') AS preferred_username,
            COALESCE(NULLIF(ra.display_name, ''), '') AS display_name,
            LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) AS status,
            COALESCE(TO_CHAR(ra.discovered_at, 'YYYY-MM-DD HH24:MI'), '') AS discovered_at,
            COALESCE(TO_CHAR(ra.last_refreshed_at, 'YYYY-MM-DD HH24:MI'), '') AS last_seen_at,
            COALESCE(NULLIF(ra.icon_url, ''), '/public/avatar.webp') AS profile_photo_url
        FROM ap_remote_actor ra
        ORDER BY
            CASE
                WHEN LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) = 'ban' THEN 0
                WHEN LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) = 'limited' THEN 1
                ELSE 2
            END,
            ra.last_refreshed_at DESC NULLS LAST,
            ra.actor_id ASC
        LIMIT 300
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| {
            let status = normalize_discovery_status(&row.status);
            ModeratorRemoteUserView {
                actor_id: row.actor_id,
                host: row.host,
                preferred_username: row.preferred_username,
                display_name: row.display_name,
                status,
                discovered_at: row.discovered_at,
                last_seen_at: row.last_seen_at,
                profile_photo_url: row.profile_photo_url,
            }
        })
        .collect()
}


#[derive(Deserialize)]
pub struct ModeratorUserRoleForm {
    pub role: String,
}

pub async fn moderator_set_user_role(
    Path(public_user_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<ModeratorUserRoleForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(next_role) = normalize_moderation_role(&form.role) else {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid role value. Allowed: user, suspended, ban",
        )
            .into_response();
    };

    if next_role == "mod" {
        return (
            StatusCode::FORBIDDEN,
            "Only the website host can assign moderator role",
        )
            .into_response();
    }

    let target_role = sqlx::query_scalar::<_, String>(
        r#"
        SELECT LOWER(COALESCE(role::TEXT, 'user'))
        FROM users
        WHERE public_id = $1
        "#,
    )
    .bind(public_user_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(target_role) = target_role else {
        return Redirect::to("/moderator").into_response();
    };

    if is_moderator_role(&target_role) {
        return (
            StatusCode::FORBIDDEN,
            "Moderators cannot update another moderator account",
        )
            .into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET role = $1
        WHERE public_id = $2
        "#,
    )
    .bind(next_role)
    .bind(public_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_set_user_role failed: {}", err);
    }

    Redirect::to("/moderator").into_response()
}





/*  -----------------------------------------------------
    |                                                   |
    | Reports section                                   |
    |                                                   |
    -----------------------------------------------------
*/

pub struct ModeratorUserView {
    pub public_id: i64,
    pub username: String,
    pub preferred_username: String,
    pub email: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub role: String,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
struct ModeratorRemoteUserRow {
    actor_id: String,
    host: String,
    preferred_username: String,
    display_name: String,
    status: String,
    discovered_at: String,
    last_seen_at: String,
    profile_photo_url: String,
}
pub struct ModeratorRemoteUserView {
    pub actor_id: String,
    pub host: String,
    pub preferred_username: String,
    pub display_name: String,
    pub status: String,
    pub discovered_at: String,
    pub last_seen_at: String,
    pub profile_photo_url: String,
}

#[derive(sqlx::FromRow)]
struct ModeratorInstanceRow {
    host: String,
    protocol: String,
    status: String,
    discovered_at: String,
    last_seen_at: String,
    remote_actor_count: i64,
}
pub struct ModeratorInstanceView {
    pub host: String,
    pub protocol: String,
    pub status: String,
    pub discovered_at: String,
    pub last_seen_at: String,
    pub remote_actor_count: i64,
}

#[derive(sqlx::FromRow)]
struct ModeratorReportRow {
    report_id: i64,
    kind: String,
    status: String,
    reason: String,
    created_at: String,
    reporter_username: String,
    target_user_public_id: Option<i64>,
    target_user_username: Option<String>,
    target_post_id: Option<i64>,
    target_message_id: Option<i64>,
    target_message_text: String,
    target_message_sender_public_id: Option<i64>,
    target_message_sender_username: Option<String>,
    target_post_soft_deleted: bool,
    target_post_author_public_id: Option<i64>,
    target_post_author_username: Option<String>,
}
pub struct ModeratorReportView {
    pub report_id: i64,
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub created_at: String,
    pub reporter_username: String,
    pub target_user_public_id: i64,
    pub target_user_username: String,
    pub target_post_id: i64,
    pub target_message_id: i64,
    pub target_message_text: String,
    pub target_message_sender_public_id: i64,
    pub target_message_sender_username: String,
    pub target_post_soft_deleted: bool,
    pub target_post_author_public_id: i64,
    pub target_post_author_username: String,
}
async fn load_moderator_reports(pool: &PgPool) -> Vec<ModeratorReportView> {
    let rows = sqlx::query_as::<_, ModeratorReportRow>(
        r#"
        SELECT
            r.report_id,
            LOWER(COALESCE(r.kind, '')) AS kind,
            LOWER(COALESCE(r.status, 'open')) AS status,
            COALESCE(NULLIF(r.reason, ''), '') AS reason,
            COALESCE(TO_CHAR(r.created_at, 'YYYY-MM-DD HH24:MI'), '') AS created_at,
            reporter.username AS reporter_username,
            target_user.public_id AS target_user_public_id,
            target_user.username AS target_user_username,
            r.target_post_id AS target_post_id,
            r.target_message_id::BIGINT AS target_message_id,
            COALESCE(NULLIF(message_row.msg, ''), '') AS target_message_text,
            message_sender.public_id AS target_message_sender_public_id,
            message_sender.username AS target_message_sender_username,
            CASE
                WHEN BTRIM(COALESCE(p.body, '')) = $1 THEN TRUE
                ELSE FALSE
            END AS target_post_soft_deleted,
            post_author.public_id AS target_post_author_public_id,
            post_author.username AS target_post_author_username
        FROM app_report r
        JOIN users reporter ON reporter.id = r.reporter_user_id
        LEFT JOIN users target_user ON target_user.id = r.target_user_id
        LEFT JOIN posts p ON p.post_id = r.target_post_id
        LEFT JOIN users post_author ON post_author.id = p.user_id
        LEFT JOIN messages message_row ON message_row.msg_id = r.target_message_id
        LEFT JOIN users message_sender ON message_sender.id = message_row.sender
        WHERE LOWER(COALESCE(r.status, 'open')) = 'open'
        ORDER BY
            CASE
                WHEN LOWER(COALESCE(r.status, 'open')) = 'open' THEN 0
                ELSE 1
            END,
            r.report_id DESC
        LIMIT 300
        "#,
    )
    .bind(MODERATOR_REDACTED_POST_TEXT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| ModeratorReportView {
            report_id: row.report_id,
            kind: row.kind,
            status: row.status,
            reason: row.reason,
            created_at: row.created_at,
            reporter_username: row.reporter_username,
            target_user_public_id: row.target_user_public_id.unwrap_or(0),
            target_user_username: row.target_user_username.unwrap_or_default(),
            target_post_id: row.target_post_id.unwrap_or(0),
            target_message_id: row.target_message_id.unwrap_or(0),
            target_message_text: row.target_message_text,
            target_message_sender_public_id: row.target_message_sender_public_id.unwrap_or(0),
            target_message_sender_username: row.target_message_sender_username.unwrap_or_default(),
            target_post_soft_deleted: row.target_post_soft_deleted,
            target_post_author_public_id: row.target_post_author_public_id.unwrap_or(0),
            target_post_author_username: row.target_post_author_username.unwrap_or_default(),
        })
        .collect()
}

pub async fn moderator_delete_report(
    Path(report_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let current_role = load_user_role(&pool, current_user_id).await;
    if !is_moderator_role(&current_role) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM app_report
        WHERE report_id = $1
        "#,
    )
    .bind(report_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("moderator_delete_report failed: {}", err);
    }

    Redirect::to("/moderator").into_response()
}

pub struct ProtocolCountView {
    pub name: String,
    pub count: i64,
}

#[derive(sqlx::FromRow)]
struct ModeratorStatsRow {
    total_users: i64,
    total_moderators: i64,
    total_suspended: i64,
    total_posts: i64,
}
pub struct ModeratorStatsView {
    pub total_users: i64,
    pub total_moderators: i64,
    pub total_suspended: i64,
    pub total_posts: i64,
    pub total_federated_instances: i64,
    pub total_discovered_instances: i64,
    pub total_limited_instances: i64,
    pub total_banned_instances: i64,
    pub total_discovered_remote_users: i64,
    pub total_limited_remote_users: i64,
    pub total_banned_remote_users: i64,
    pub protocol_breakdown: Vec<ProtocolCountView>,
}


/*  -----------------------------------------------------
    |                                                   |
    | Federation section                                |
    |                                                   |
    -----------------------------------------------------
*/

fn normalize_instance_host(raw: &str) -> String {
    raw.trim()
        .trim_matches('/')
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'))
        .collect()
}

fn normalize_discovery_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "discover" | "discovered" | "authorized" | "allow" | "allowed" => "discovered".to_string(),
        "limit" | "limited" => "limited".to_string(),
        "ban" | "banned" | "blocked" | "block" => "ban".to_string(),
        _ => "discovered".to_string(),
    }
}


async fn load_moderator_instances(pool: &PgPool) -> Vec<ModeratorInstanceView> {
    let rows = sqlx::query_as::<_, ModeratorInstanceRow>(
        r#"
        SELECT
            di.host,
            COALESCE(NULLIF(di.protocol, ''), 'Other ActivityPub') AS protocol,
            LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) AS status,
            COALESCE(TO_CHAR(di.discovered_at, 'YYYY-MM-DD HH24:MI'), '') AS discovered_at,
            COALESCE(TO_CHAR(di.last_seen_at, 'YYYY-MM-DD HH24:MI'), '') AS last_seen_at,
            COALESCE((
                SELECT COUNT(*)::BIGINT
                FROM ap_remote_actor ra
                WHERE ra.actor_id ILIKE ('https://' || di.host || '/%')
                   OR ra.actor_id ILIKE ('http://' || di.host || '/%')
            ), 0) AS remote_actor_count
        FROM discovered_instance di
        ORDER BY
            CASE
                WHEN LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) = 'ban' THEN 0
                WHEN LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) = 'limited' THEN 1
                ELSE 2
            END,
            di.last_seen_at DESC,
            di.host ASC
        LIMIT 300
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| ModeratorInstanceView {
            host: row.host,
            protocol: row.protocol,
            status: normalize_discovery_status(&row.status),
            discovered_at: row.discovered_at,
            last_seen_at: row.last_seen_at,
            remote_actor_count: row.remote_actor_count,
        })
        .collect()
}

pub async fn moderator_update_instance_status(
    Path(host): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    if let Err(response) = ensure_moderator_session(&pool, &session).await {
        return response;
    }

    let host = normalize_instance_host(&host);
    if host.is_empty() {
        return Redirect::to("/moderator?instance_status=invalid_host").into_response();
    }
    let status = normalize_discovery_status(query.get("status").map(String::as_str).unwrap_or(""));

    match sqlx::query(
        r#"
        INSERT INTO discovered_instance (host, protocol, status, discovered_at, last_seen_at)
        VALUES ($1, 'Other ActivityPub', $2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (host) DO UPDATE
        SET status = $2,
            last_seen_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(&host)
    .bind(&status)
    .execute(&pool)
    .await
    {
        Ok(_) => Redirect::to(&format!("/moderator?instance_status={}", status)).into_response(),
        Err(err) => {
            tracing::warn!("moderator_update_instance_status failed: {}", err);
            Redirect::to("/moderator?instance_status=save_error").into_response()
        }
    }
}


#[derive(Deserialize)]
pub struct ModeratorRemoteUserStatusForm {
    pub actor_id: Option<String>,
    pub status: Option<String>,
}

pub async fn moderator_update_remote_user_status(
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<ModeratorRemoteUserStatusForm>,
) -> impl IntoResponse {
    if let Err(response) = ensure_moderator_session(&pool, &session).await {
        return response;
    }

    let actor_id = form.actor_id.unwrap_or_default().trim().to_string();
    if actor_id.is_empty() {
        return Redirect::to("/moderator?remote_user_status=invalid_actor").into_response();
    }
    let status = normalize_discovery_status(form.status.as_deref().unwrap_or(""));

    match sqlx::query(
        r#"
        UPDATE ap_remote_actor
        SET status = $2,
            last_refreshed_at = CURRENT_TIMESTAMP
        WHERE actor_id = $1
        "#,
    )
    .bind(&actor_id)
    .bind(&status)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => {
            Redirect::to(&format!("/moderator?remote_user_status={}", status)).into_response()
        }
        Ok(_) => Redirect::to("/moderator?remote_user_status=invalid_actor").into_response(),
        Err(err) => {
            tracing::warn!("moderator_update_remote_user_status failed: {}", err);
            Redirect::to("/moderator?remote_user_status=save_error").into_response()
        }
    }
}










// Change location
pub async fn report_user(
    Path(public_user_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let target_user = sqlx::query_as::<_, (i32, bool)>(
        r#"
        SELECT
            id,
            LOWER(username) = LOWER($2) AS is_instavox_team
        FROM users
        WHERE public_id = $1
        "#,
    )
    .bind(public_user_id)
    .bind(MODERATOR_TEAM_USERNAME)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some((target_user_id, is_instavox_team)) = target_user else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if is_instavox_team {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    if target_user_id != current_user_id {
        if let Err(err) = sqlx::query(
            r#"
            INSERT INTO app_report (
                reporter_user_id,
                kind,
                target_user_id,
                target_post_id,
                reason,
                status,
                created_at,
                modified_at
            )
            SELECT
                $1,
                'user',
                $2,
                NULL,
                '',
                'open',
                NOW(),
                NOW()
            WHERE NOT EXISTS (
                SELECT 1
                FROM app_report
                WHERE reporter_user_id = $1
                  AND LOWER(COALESCE(kind, '')) = 'user'
                  AND target_user_id = $2
                  AND LOWER(COALESCE(status, 'open')) = 'open'
            )
            "#,
        )
        .bind(current_user_id)
        .bind(target_user_id)
        .execute(&pool)
        .await
        {
            tracing::warn!("report_user failed: {}", err);
        }
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn report_post(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let target_user_id = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT user_id
        FROM posts
        WHERE post_id = $1
        "#,
    )
    .bind(post_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(target_user_id) = target_user_id else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if target_user_id != current_user_id {
        if let Err(err) = sqlx::query(
            r#"
            INSERT INTO app_report (
                reporter_user_id,
                kind,
                target_user_id,
                target_post_id,
                reason,
                status,
                created_at,
                modified_at
            )
            SELECT
                $1,
                'post',
                $2,
                $3,
                '',
                'open',
                NOW(),
                NOW()
            WHERE NOT EXISTS (
                SELECT 1
                FROM app_report
                WHERE reporter_user_id = $1
                  AND LOWER(COALESCE(kind, '')) = 'post'
                  AND target_post_id = $3
                  AND LOWER(COALESCE(status, 'open')) = 'open'
            )
            "#,
        )
        .bind(current_user_id)
        .bind(target_user_id)
        .bind(post_id)
        .execute(&pool)
        .await
        {
            tracing::warn!("report_post failed: {}", err);
        }
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn report_message(
    Path(msg_id): Path<i32>,
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let fetch_request = is_fetch_request(&headers);
    let Some(current_user_id) = session_user_id(&session).await else {
        if fetch_request {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        return Redirect::to("/login").into_response();
    };

    let message_target = sqlx::query_as::<_, (i32, bool)>(
        r#"
        SELECT
            m.sender AS target_user_id,
            CASE
                WHEN LOWER(COALESCE(c.chat_type, '')) = 'group' THEN EXISTS(
                    SELECT 1
                    FROM chat_member cm
                    WHERE cm.chat_id = c.chat_id
                      AND cm.user_id = $2
                )
                WHEN LOWER(COALESCE(c.chat_type, 'friendship')) = 'friendship' THEN EXISTS(
                    SELECT 1
                    FROM relationship r
                    WHERE r.friendship_id = c.chat_title
                      AND (r.sender_id = $2 OR r.receiver_id = $2)
                      AND LOWER(COALESCE(r.status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
                )
                ELSE FALSE
            END AS can_access
        FROM messages m
        JOIN chat c ON c.chat_id = m.chat_id
        WHERE m.msg_id = $1
        "#,
    )
    .bind(msg_id)
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some((target_user_id, can_access)) = message_target else {
        if fetch_request {
            return StatusCode::NOT_FOUND.into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if !can_access {
        if fetch_request {
            return StatusCode::FORBIDDEN.into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    if target_user_id != current_user_id
        && let Err(err) = sqlx::query(
            r#"
            INSERT INTO app_report (
                reporter_user_id,
                kind,
                target_user_id,
                target_post_id,
                target_message_id,
                reason,
                status,
                created_at,
                modified_at
            )
            SELECT
                $1,
                'message',
                $2,
                NULL,
                $3,
                '',
                'open',
                NOW(),
                NOW()
            WHERE NOT EXISTS (
                SELECT 1
                FROM app_report
                WHERE reporter_user_id = $1
                  AND LOWER(COALESCE(kind, '')) = 'message'
                  AND target_message_id = $3
                  AND LOWER(COALESCE(status, 'open')) = 'open'
            )
            "#,
        )
        .bind(current_user_id)
        .bind(target_user_id)
        .bind(msg_id)
        .execute(&pool)
        .await
    {
        tracing::warn!("report_message failed: {}", err);
    }

    if fetch_request {
        return Json(serde_json::json!({
            "success": true,
            "message_id": msg_id
        }))
        .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn delete_message(
    Path(msg_id): Path<i32>,
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let fetch_request = is_fetch_request(&headers);
    let Some(current_user_id) = session_user_id(&session).await else {
        if fetch_request {
            return (StatusCode::UNAUTHORIZED, "Login required").into_response();
        }
        return Redirect::to("/login").into_response();
    };

    if msg_id <= 0 {
        if fetch_request {
            return (StatusCode::BAD_REQUEST, "Invalid message id").into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let delete_result = sqlx::query_scalar::<_, i32>(
        r#"
        DELETE FROM messages
        WHERE msg_id = $1
          AND sender = $2
        RETURNING chat_id
        "#,
    )
    .bind(msg_id)
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await;

    match delete_result {
        Ok(Some(_)) => {
            if fetch_request {
                return (StatusCode::OK, "Message deleted").into_response();
            }
        }
        Ok(None) => {
            if fetch_request {
                return (
                    StatusCode::FORBIDDEN,
                    "You can only delete your own messages",
                )
                    .into_response();
            }
        }
        Err(err) => {
            tracing::warn!("delete_message failed: {}", err);
            if fetch_request {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to delete message",
                )
                    .into_response();
            }
        }
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn api_posts_page(
    Query(query): Query<FeedPageQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let limit = normalize_feed_limit(query.limit);
    let response = load_index_posts_segment(
        &pool,
        current_user_id,
        query.before_post_id,
        query.after_post_id,
        limit,
    )
    .await;
    Json(response).into_response()
}

fn normalize_notification_limit(raw_limit: Option<i64>) -> i64 {
    let requested = raw_limit.unwrap_or(NOTIFICATION_PAGE_DEFAULT_LIMIT);
    requested.clamp(1, NOTIFICATION_PAGE_MAX_LIMIT)
}

pub async fn api_notifications_page(
    Query(query): Query<NotificationPageQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let limit = normalize_notification_limit(query.limit);
    let rows = match sqlx::query_as::<_, HeaderNotificationRow>(
        r#"
        SELECT
            notification_id,
            COALESCE(NULLIF(title, ''), 'Notification') AS title,
            COALESCE(NULLIF(body, ''), '') AS body,
            COALESCE(NULLIF(link_url, ''), '/') AS link_url,
            CASE
                WHEN LOWER(COALESCE(is_read::TEXT, 'false')) IN ('t', 'true', '1', 'yes', 'y')
                    THEN TRUE
                ELSE FALSE
            END AS is_read,
            COALESCE(created_at::TEXT, '') AS created_at,
            COALESCE(message_count, 1)::BIGINT AS message_count
        FROM app_notification
        WHERE user_id = $1
          AND ($2::BIGINT IS NULL OR notification_id < $2)
        ORDER BY notification_id DESC
        LIMIT $3
        "#,
    )
    .bind(current_user_id)
    .bind(query.before_notification_id.filter(|value| *value > 0))
    .bind(limit + 1)
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("api_notifications_page query failed: {}", err);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let has_more = rows.len() as i64 > limit;
    let page_rows = if has_more {
        rows.into_iter().take(limit as usize).collect::<Vec<_>>()
    } else {
        rows
    };

    let notifications = page_rows
        .iter()
        .map(|row| NotificationPageItem {
            notification_id: row.notification_id,
            title: row.title.clone(),
            body: row.body.clone(),
            link_url: row.link_url.clone(),
            created_at: row.created_at.clone(),
            is_unread: !row.is_read,
            message_count: row.message_count.max(1),
        })
        .collect::<Vec<_>>();

    let next_before_notification_id = notifications.last().map(|item| item.notification_id);

    Json(NotificationPageResponse {
        notifications,
        has_more,
        next_before_notification_id,
    })
    .into_response()
}

pub async fn api_profile_posts_page(
    Path(public_user_id): Path<i64>,
    Query(query): Query<FeedPageQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let profile_user_id = match sqlx::query_scalar::<_, i32>(
        r#"
        SELECT id
        FROM users
        WHERE public_id = $1
        "#,
    )
    .bind(public_user_id)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            tracing::warn!(
                "api_profile_posts_page resolve profile {} failed: {}",
                public_user_id,
                err
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let current_user_id = session_user_id(&session).await;
    let limit = normalize_feed_limit(query.limit);
    let response = load_profile_posts_segment(
        &pool,
        profile_user_id,
        current_user_id,
        query.before_post_id,
        query.after_post_id,
        limit,
    )
    .await;

    Json(response).into_response()
}

pub async fn create_post(
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let draft = match read_post_draft_from_multipart(multipart, current_user_id, None).await {
        Ok(draft) => draft,
        Err((status, message)) => return (status, message).into_response(),
    };

    let post_id = match insert_post_from_draft(&pool, current_user_id, None, draft).await {
        Ok(post_id) => post_id,
        Err(err) => {
            tracing::warn!("create_post failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to publish your post right now",
            )
                .into_response();
        }
    };

    let federation_pool = pool.clone();
    tokio::spawn(async move {
        if let Err(err) = crate::federation::send_post_to_subscribed_remote_inboxes(
            &federation_pool,
            current_user_id,
            post_id,
        )
        .await
        {
            tracing::warn!(
                "post federation delivery failed (post_id={}, user_id={}): {}",
                post_id,
                current_user_id,
                err
            );
        }
    });

    Redirect::to("/").into_response()
}

#[derive(Deserialize)]
pub struct LinkImageQuery {
    pub url: String,
}

#[derive(Serialize)]
pub struct LinkImageResponse {
    pub image_url: String,
}

pub async fn api_link_first_image(Query(query): Query<LinkImageQuery>) -> impl IntoResponse {
    let image_url = resolve_first_image_from_link(&query.url).await;
    Json(LinkImageResponse { image_url })
}

#[derive(Deserialize)]
pub struct EditPostForm {
    pub text: String,
    pub visibility: Option<String>,
}

pub async fn edit_post(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<EditPostForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let owner = sqlx::query_as::<_, PostOwnerRow>(
        r#"
        SELECT user_id
        FROM posts
        WHERE post_id = $1
        "#,
    )
    .bind(post_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(owner) = owner else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if owner.user_id != current_user_id {
        return StatusCode::FORBIDDEN.into_response();
    }

    let text = form.text.trim().to_string();
    if text.len() > MAX_POST_TEXT_LENGTH {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Post text must be at most {} characters",
                MAX_POST_TEXT_LENGTH
            ),
        )
            .into_response();
    }

    let has_images = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::BIGINT
        FROM post_image
        WHERE post_id = $1
        "#,
    )
    .bind(post_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(0)
        > 0;

    if text.is_empty() && !has_images {
        return (
            StatusCode::BAD_REQUEST,
            "A post requires text or at least one image",
        )
            .into_response();
    }

    let detected_link = extract_first_link_from_text(&text).unwrap_or_default();
    let visibility = form
        .visibility
        .as_deref()
        .map(normalize_post_visibility)
        .unwrap_or(POST_VISIBILITY_PUBLIC);
    if let Err(err) = sqlx::query(
        r#"
        UPDATE posts
        SET body = $1,
            link_url = NULLIF($2, ''),
            visibility = $3,
            updated_at = NOW()
        WHERE post_id = $4
          AND user_id = $5
        "#,
    )
    .bind(&text)
    .bind(&detected_link)
    .bind(visibility)
    .bind(post_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("edit_post update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to update your post right now",
        )
            .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn delete_post(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM posts
        WHERE post_id = $1
          AND user_id = $2
        "#,
    )
    .bind(post_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("delete_post failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to delete post right now",
        )
            .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn toggle_post_like(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let (post_exists, is_soft_deleted) = load_post_visibility_state(&pool, post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and can no longer receive likes.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (StatusCode::FORBIDDEN, "You cannot interact with this post.").into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let result = async {
        let mut tx = pool.begin().await?;
        let lock_key = format!("post-reaction:{post_id}:{current_user_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *tx)
            .await?;

        let was_liked = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM post_like
                WHERE post_id = $1
                  AND user_id = $2
            )
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM post_like
            WHERE post_id = $1
              AND user_id = $2
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM post_dislike
            WHERE post_id = $1
              AND user_id = $2
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .execute(&mut *tx)
        .await?;

        if !was_liked {
            sqlx::query(
                r#"
                INSERT INTO post_like (post_id, user_id, created_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (post_id, user_id) DO NOTHING
                "#,
            )
            .bind(post_id)
            .bind(current_user_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await
    }
    .await;

    if let Err(err) = result {
        tracing::warn!("toggle_post_like transaction failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn toggle_post_dislike(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let (post_exists, is_soft_deleted) = load_post_visibility_state(&pool, post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and can no longer receive dislikes.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (StatusCode::FORBIDDEN, "You cannot interact with this post.").into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let result = async {
        let mut tx = pool.begin().await?;
        let lock_key = format!("post-reaction:{post_id}:{current_user_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *tx)
            .await?;

        let was_disliked = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM post_dislike
                WHERE post_id = $1
                  AND user_id = $2
            )
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM post_like
            WHERE post_id = $1
              AND user_id = $2
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM post_dislike
            WHERE post_id = $1
              AND user_id = $2
            "#,
        )
        .bind(post_id)
        .bind(current_user_id)
        .execute(&mut *tx)
        .await?;

        if !was_disliked {
            sqlx::query(
                r#"
                INSERT INTO post_dislike (post_id, user_id, created_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (post_id, user_id) DO NOTHING
                "#,
            )
            .bind(post_id)
            .bind(current_user_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await
    }
    .await;

    if let Err(err) = result {
        tracing::warn!("toggle_post_dislike transaction failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn toggle_comment_like(
    session: Session,
    State(pool): State<PgPool>,
    Path(comment_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let comment_post = sqlx::query_as::<_, PostCommentPostRow>(
        r#"
        SELECT post_id, user_id AS comment_owner_user_id
        FROM post_comment
        WHERE comment_id = $1
        "#,
    )
    .bind(comment_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(comment_post) = comment_post else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    let (post_exists, is_soft_deleted) =
        load_post_visibility_state(&pool, comment_post.post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and can no longer receive likes.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, comment_post.post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (
                StatusCode::FORBIDDEN,
                "You cannot interact with comments on this post.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let deleted = sqlx::query(
        r#"
        DELETE FROM post_comment_like
        WHERE comment_id = $1
          AND user_id = $2
        "#,
    )
    .bind(comment_id)
    .bind(current_user_id)
    .execute(&pool)
    .await;

    match deleted {
        Ok(result) if result.rows_affected() == 0 => {
            let inserted = sqlx::query(
                r#"
                INSERT INTO post_comment_like (comment_id, user_id, created_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (comment_id, user_id) DO NOTHING
                "#,
            )
            .bind(comment_id)
            .bind(current_user_id)
            .execute(&pool)
            .await;

            match inserted {
                Ok(insert_result) => {
                    if insert_result.rows_affected() > 0
                        && comment_post.comment_owner_user_id != current_user_id
                    {
                        let likes_by_other_people = sqlx::query_scalar::<_, i64>(
                            r#"
                            SELECT COUNT(*)::BIGINT
                            FROM post_comment_like
                            WHERE comment_id = $1
                              AND user_id <> $2
                            "#,
                        )
                        .bind(comment_id)
                        .bind(comment_post.comment_owner_user_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap_or(1)
                        .max(1);

                        let body = if likes_by_other_people > 1 {
                            format!("{} people liked your comment", likes_by_other_people)
                        } else {
                            "Someone liked your comment".to_string()
                        };

                        let link_url =
                            format!("/posts/{}#comment-{}", comment_post.post_id, comment_id);
                        if let Err(err) = create_comment_like_notification(
                            &pool,
                            comment_post.comment_owner_user_id,
                            &body,
                            &link_url,
                        )
                        .await
                        {
                            tracing::warn!(
                                "toggle_comment_like notification failed (comment_id={}): {}",
                                comment_id,
                                err
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("toggle_comment_like insert failed: {}", err);
                }
            }
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("toggle_comment_like delete failed: {}", err),
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn toggle_post_share(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let (post_exists, is_soft_deleted) = load_post_visibility_state(&pool, post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and can no longer be shared.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (StatusCode::FORBIDDEN, "You cannot interact with this post.").into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let deleted = sqlx::query(
        r#"
        DELETE FROM post_share
        WHERE post_id = $1
          AND user_id = $2
        "#,
    )
    .bind(post_id)
    .bind(current_user_id)
    .execute(&pool)
    .await;

    match deleted {
        Ok(result) if result.rows_affected() == 0 => {
            if let Err(err) = sqlx::query(
                r#"
                INSERT INTO post_share (post_id, user_id, created_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (post_id, user_id) DO NOTHING
                "#,
            )
            .bind(post_id)
            .bind(current_user_id)
            .execute(&pool)
            .await
            {
                tracing::warn!("toggle_post_share insert failed: {}", err);
            }
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("toggle_post_share delete failed: {}", err),
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

#[derive(Deserialize)]
pub struct AddPostCommentForm {
    pub body: String,
    pub reply_to_comment_id: Option<i64>,
}

#[derive(Serialize)]
pub struct AddPostCommentResponse {
    pub comment_id: i64,
    pub created_at: String,
    pub reply_parent_comment_id: i64,
    pub reply_to_comment_id: Option<i64>,
    pub reply_to_body_preview: String,
    pub reply_to_username: String,
}

pub async fn add_post_comment(
    session: Session,
    State(pool): State<PgPool>,
    Path(post_id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<AddPostCommentForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let comment = form.body.trim().to_string();
    if comment.is_empty() {
        return (StatusCode::BAD_REQUEST, "Comment cannot be empty").into_response();
    }
    if comment.len() > MAX_POST_COMMENT_LENGTH {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Comment must be at most {} characters",
                MAX_POST_COMMENT_LENGTH
            ),
        )
            .into_response();
    }

    let (post_exists, is_soft_deleted) = load_post_visibility_state(&pool, post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and can no longer receive comments.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (StatusCode::FORBIDDEN, "You cannot interact with this post.").into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let mut reply_to_comment_id = form.reply_to_comment_id.filter(|value| *value > 0);
    let mut reply_parent_comment_id: i64 = 0;
    let mut reply_to_username = String::new();
    let mut reply_to_body_preview = String::new();
    let mut reply_target_user_id: Option<i32> = None;
    if let Some(reply_id) = reply_to_comment_id {
        let reply_preview = sqlx::query_as::<_, (i32, String, String)>(
            r#"
            SELECT
                c.user_id,
                u.username,
                COALESCE(NULLIF(c.body, ''), '') AS body
            FROM post_comment c
            JOIN users u ON u.id = c.user_id
            WHERE c.comment_id = $1
              AND c.post_id = $2
            LIMIT 1
            "#,
        )
        .bind(reply_id)
        .bind(post_id)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();

        if let Some((target_user_id, username, body)) = reply_preview {
            reply_target_user_id = Some(target_user_id);
            reply_parent_comment_id = reply_id;
            reply_to_username = username;
            reply_to_body_preview = truncate_preview(body.trim(), 120);
        } else {
            if is_fetch_request(&headers) {
                return (StatusCode::BAD_REQUEST, "Invalid reply target.").into_response();
            }
            let redirect_to = redirect_back_path(&headers);
            return Redirect::to(&redirect_to).into_response();
        }
    } else {
        reply_to_comment_id = None;
    }

    let inserted = sqlx::query_as::<_, (i64, String)>(
        r#"
        INSERT INTO post_comment (post_id, user_id, body, reply_to_comment_id, created_at, updated_at)
        VALUES ($1, $2, $3, $4, NOW(), NOW())
        RETURNING comment_id, COALESCE(created_at::TEXT, NOW()::TEXT)
        "#,
    )
    .bind(post_id)
    .bind(current_user_id)
    .bind(&comment)
    .bind(reply_to_comment_id)
    .fetch_one(&pool)
    .await;

    let (comment_id, created_at) = match inserted {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!("add_post_comment insert failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to add comment right now",
            )
                .into_response();
        }
    };

    let post_owner_user_id = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT user_id
        FROM posts
        WHERE post_id = $1
        LIMIT 1
        "#,
    )
    .bind(post_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let sender_username = load_user_identity(&pool, current_user_id)
        .await
        .map(|(username, _)| username)
        .unwrap_or_else(|| "someone".to_string());
    let comment_preview = truncate_preview(comment.trim(), 140);
    let comment_preview_fragment = if comment_preview.is_empty() {
        String::new()
    } else {
        format!(": \"{}\"", comment_preview)
    };
    let notification_link = format!("/posts/{}#comment-{}", post_id, comment_id);
    let mut notification_targets: HashMap<i32, (&str, String, String)> = HashMap::new();

    if let Some(owner_user_id) = post_owner_user_id
        && owner_user_id != current_user_id
    {
        notification_targets.insert(
            owner_user_id,
            (
                "post_comment",
                "New comment on your post".to_string(),
                format!(
                    "@{} commented on your post{}",
                    sender_username, comment_preview_fragment
                ),
            ),
        );
    }

    if let Some(target_user_id) = reply_target_user_id
        && target_user_id != current_user_id
    {
        notification_targets.insert(
            target_user_id,
            (
                "comment_reply",
                "New reply to your comment".to_string(),
                format!(
                    "@{} replied to your comment{}",
                    sender_username, comment_preview_fragment
                ),
            ),
        );
    }

    for (target_user_id, (kind, title, body)) in notification_targets {
        if let Err(err) = create_notification(
            &pool,
            target_user_id,
            kind,
            &title,
            &body,
            &notification_link,
        )
        .await
        {
            tracing::warn!(
                "add_post_comment notification failed (post_id={}, comment_id={}, target_user_id={}, kind={}): {}",
                post_id,
                comment_id,
                target_user_id,
                kind,
                err
            );
        }
    }

    if is_fetch_request(&headers) {
        return Json(AddPostCommentResponse {
            comment_id,
            created_at,
            reply_parent_comment_id,
            reply_to_comment_id,
            reply_to_body_preview,
            reply_to_username,
        })
        .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}