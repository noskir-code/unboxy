use askama::Template;
use axum::Form;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;
use sqlx::PgPool;
use axum::extract::{Multipart, Path, Query, State};
use tower_sessions::Session;
use crate::handlers::image_processing::read_uploaded_image;
use crate::handlers::notifications::{HeaderNotificationView, load_header_notifications};
use crate::handlers::post::{IndexPostView, POST_VISIBILITY_PRIVATE, POST_VISIBILITY_PUBLIC, insert_post_from_draft, load_community_posts, read_post_draft_from_multipart};
use crate::handlers::searching::{load_communities, load_community_by_slug, load_joined_communities, parse_local_username_lookup};
use crate::handlers::session::{is_acting_as_team_session, session_public_user_id, session_string, session_user_id};
use crate::handlers::user::{DEFAULT_PROFILE_PHOTO_URL, SelectUploadedImageForm, crop_style_from_form, is_owned_uploaded_image, list_uploaded_images, load_is_moderator, local_profile_domain, save_settings_image_file, store_uploaded_image_record};
use crate::routes::{empty_community_page_view, render_template_response};
use crate::truncate_chars;

const MAX_COMMUNITY_NAME_CHARS: usize = 120;
const MAX_COMMUNITY_SLUG_CHARS: usize = 80;
const MAX_COMMUNITY_DESCRIPTION_CHARS: usize = 1_200;
const MAX_COMMUNITY_RULE_TITLE_CHARS: usize = 140;
const MAX_COMMUNITY_RULE_BODY_CHARS: usize = 2_000;

// List all created and discovered communities
#[derive(Deserialize)]
pub struct CommunitiesQuery {
    pub sort: Option<String>,
    pub create_status: Option<String>,
}
#[derive(Template)]
#[template(path = "main/communities.html")]
#[allow(dead_code)]
pub struct CommunitiesTemplate {
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
    pub create_message: String,
    pub create_success: bool,
    pub joined_communities: Vec<CommunityPageView>,
    pub communities: Vec<CommunityPageView>,
}


// Community page
#[derive(Template)]
#[template(path = "models/community.html")]
#[allow(dead_code)]
pub struct CommunityTemplate {
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
    pub active_sort: String,
    pub community_rules: Vec<CommunityRuleView>,
    pub community_moderators: Vec<CommunityModeratorView>,
    pub found: bool,
    pub can_manage: bool,
    pub can_team_moderate: bool,
    pub is_member: bool,
    pub can_quit: bool,
    pub can_join: bool,
    pub is_ignored: bool,
    pub can_view_posts: bool,
    pub can_create_post: bool,
    pub posts: Vec<IndexPostView>,
    pub community: CommunityPageView,
}






/*  -----------------------------------------------------
    |                                                   |
    | Base section                                      |
    |                                                   |
    -----------------------------------------------------
*/

pub struct CommunityPageView {
    pub community_id: i64,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub visibility: String,
    pub member_count: i64,
    pub post_count: i64,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub owner_user_id: i32,
    pub owner_username: String,
    pub created_at: String,
}


pub async fn communities(
    Query(query): Query<CommunitiesQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let active_sort = normalize_community_sort(query.sort.as_deref());

    let (create_message, create_success) = match query.create_status.as_deref() {
        Some("created") => ("Community page created.".to_string(), true),
        Some("deleted") => ("Community deleted.".to_string(), true),
        Some("banned") => ("Community banned.".to_string(), true),
        Some("name_required") => ("Community name is required.".to_string(), false),
        Some("invalid_slug") => (
            "Community slug is invalid. Use letters and numbers.".to_string(),
            false,
        ),
        Some("slug_unavailable") => (
            "Unable to allocate a unique community slug right now.".to_string(),
            false,
        ),
        Some("create_failed") => (
            "Unable to create community right now. Please retry.".to_string(),
            false,
        ),
        Some("moderation_failed") => (
            "Unable to moderate community right now. Please retry.".to_string(),
            false,
        ),
        _ => (String::new(), false),
    };
    let joined_communities = match current_user_id {
        Some(user_id) if user_id > 0 => load_joined_communities(&pool, user_id).await,
        _ => Vec::new(),
    };
    let mut communities = load_communities(&pool, active_sort, current_user_id).await;
    if !joined_communities.is_empty() {
        let joined_ids = joined_communities
            .iter()
            .map(|community| community.community_id)
            .collect::<std::collections::BTreeSet<_>>();
        communities.retain(|community| !joined_ids.contains(&community.community_id));
    }

    let template = CommunitiesTemplate {
        title: "Community".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        create_message,
        create_success,
        joined_communities,
        communities,
    };

    render_template_response(&template)
}

#[derive(Deserialize)]
pub struct CreateCommunityForm {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
}
pub async fn create_community(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<CreateCommunityForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let trimmed_name = payload.name.trim();
    if trimmed_name.is_empty() {
        return Redirect::to("/communities?create_status=name_required").into_response();
    }

    let name = truncate_chars(trimmed_name, MAX_COMMUNITY_NAME_CHARS);
    if name.trim().is_empty() {
        return Redirect::to("/communities?create_status=name_required").into_response();
    }

    let requested_slug = payload.slug.unwrap_or_default();
    let Some(slug) = allocate_unique_community_slug(&pool, &requested_slug, &name).await else {
        let status = if slugify_community_value(&requested_slug).is_empty()
            && !requested_slug.trim().is_empty()
        {
            "invalid_slug"
        } else {
            "slug_unavailable"
        };
        return Redirect::to(&format!("/communities?create_status={}", status)).into_response();
    };

    let description = truncate_chars(
        payload.description.unwrap_or_default().trim(),
        MAX_COMMUNITY_DESCRIPTION_CHARS,
    );

    let insert_result = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO community_page (
            slug,
            name,
            description,
            profile_photo_url,
            owner_user_id,
            visibility,
            status,
            created_at,
            updated_at
        )
        VALUES (
            $1,
            $2,
            $3,
            '/public/avatar.webp',
            $4,
            'public',
            'active',
            CURRENT_TIMESTAMP,
            CURRENT_TIMESTAMP
        )
        RETURNING community_id
        "#,
    )
    .bind(&slug)
    .bind(&name)
    .bind(&description)
    .bind(current_user_id)
    .fetch_one(&pool)
    .await;

    let community_id = match insert_result {
        Ok(id) => id,
        Err(_) => return Redirect::to("/communities?create_status=create_failed").into_response(),
    };

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO community_member (community_id, user_id, joined_at)
        VALUES ($1, $2, CURRENT_TIMESTAMP)
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .bind(community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "create_community owner membership insert failed (community_id={}): {}",
            community_id,
            err
        );
        return Redirect::to("/communities?create_status=create_failed").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO community_moderator (community_id, user_id, granted_by_user_id, granted_at)
        VALUES ($1, $2, $2, CURRENT_TIMESTAMP)
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .bind(community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "create_community owner moderator insert failed (community_id={}): {}",
            community_id,
            err
        );
        return Redirect::to("/communities?create_status=create_failed").into_response();
    }

    Redirect::to(&format!("/community/{}/settings?status=created", slug)).into_response()
}


pub async fn join_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(community) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM community_ignore
        WHERE community_id = $1
          AND user_id = $2
        "#,
    )
    .bind(community.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "join_community ignore cleanup failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO community_member (community_id, user_id, joined_at)
        VALUES ($1, $2, CURRENT_TIMESTAMP)
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .bind(community.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "join_community failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to join community right now",
        )
            .into_response();
    }

    Redirect::to(&format!("/community/{}", community.slug)).into_response()
}


pub async fn quit_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(community) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if current_user_id == community.owner_user_id {
        return Redirect::to(&format!("/community/{}", community.slug)).into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM community_moderator
        WHERE community_id = $1
          AND user_id = $2
        "#,
    )
    .bind(community.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "quit_community moderator cleanup failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM community_member
        WHERE community_id = $1
          AND user_id = $2
        "#,
    )
    .bind(community.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "quit_community failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
    }

    Redirect::to("/communities").into_response()
}


pub async fn toggle_ignore_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(community) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let is_ignored = is_community_ignored(&pool, community.community_id, current_user_id).await;

    let result = if is_ignored {
        sqlx::query(
            r#"
            DELETE FROM community_ignore
            WHERE community_id = $1
              AND user_id = $2
            "#,
        )
        .bind(community.community_id)
        .bind(current_user_id)
        .execute(&pool)
        .await
    } else {
        sqlx::query(
            r#"
            INSERT INTO community_ignore (community_id, user_id, ignored_at)
            VALUES ($1, $2, CURRENT_TIMESTAMP)
            ON CONFLICT (community_id, user_id) DO NOTHING
            "#,
        )
        .bind(community.community_id)
        .bind(current_user_id)
        .execute(&pool)
        .await
    };

    if let Err(err) = result {
        tracing::warn!(
            "toggle_ignore_community failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
    }

    Redirect::to(&format!("/community/{}", community.slug)).into_response()
}


pub async fn create_community_post(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(community) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let can_manage = current_user_id == community.owner_user_id;
    let is_member =
        can_manage || is_community_member(&pool, community.community_id, current_user_id).await;
    if !is_member {
        return StatusCode::FORBIDDEN.into_response();
    }

    let draft = match read_post_draft_from_multipart(
        multipart,
        current_user_id,
        Some(POST_VISIBILITY_PUBLIC),
    )
    .await
    {
        Ok(draft) => draft,
        Err((status, message)) => return (status, message).into_response(),
    };

    if let Err(err) =
        insert_post_from_draft(&pool, current_user_id, Some(community.community_id), draft).await
    {
        tracing::warn!(
            "create_community_post failed (community_id={}, user_id={}): {}",
            community.community_id,
            current_user_id,
            err
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to publish community post right now",
        )
            .into_response();
    }

    Redirect::to(&format!("/community/{}", community.slug)).into_response()
}


#[derive(sqlx::FromRow)]
struct CommunityRuleRow {
    rule_id: i64,
    title: String,
    body: String,
    sort_order: i32,
}
pub struct CommunityRuleView {
    pub rule_id: i64,
    pub title: String,
    pub body: String,
    pub sort_order: i32,
}



/*  -----------------------------------------------------
    |                                                   |
    | Admin section                                     |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Deserialize)]
pub struct AddCommunityRuleForm {
    pub title: String,
    pub body: Option<String>,
    pub sort_order: Option<i32>,
}
pub async fn add_community_rule(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<AddCommunityRuleForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let title = truncate_chars(payload.title.trim(), MAX_COMMUNITY_RULE_TITLE_CHARS);
    if title.trim().is_empty() {
        return Redirect::to(&format!(
            "/community/{}/settings?status=rule_title_required",
            existing.slug
        ))
        .into_response();
    }
    let body = truncate_chars(
        payload.body.unwrap_or_default().trim(),
        MAX_COMMUNITY_RULE_BODY_CHARS,
    );
    let sort_order = match payload.sort_order {
        Some(value) => value.max(0),
        None => sqlx::query_scalar::<_, i32>(
            r#"
            SELECT COALESCE(MAX(sort_order), -1) + 1
            FROM community_rule
            WHERE community_id = $1
            "#,
        )
        .bind(existing.community_id)
        .fetch_one(&pool)
        .await
        .unwrap_or(0),
    };

    match sqlx::query(
        r#"
        INSERT INTO community_rule (
            community_id,
            title,
            body,
            sort_order,
            created_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        "#,
    )
    .bind(existing.community_id)
    .bind(title)
    .bind(body)
    .bind(sort_order)
    .execute(&pool)
    .await
    {
        Ok(_) => Redirect::to(&format!(
            "/community/{}/settings?status=rule_added",
            existing.slug
        ))
        .into_response(),
        Err(err) => {
            tracing::warn!(
                "add_community_rule failed (community_id={}): {}",
                existing.community_id,
                err
            );
            Redirect::to(&format!(
                "/community/{}/settings?status=rule_failed",
                existing.slug
            ))
            .into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateCommunityRuleForm {
    pub title: String,
    pub body: Option<String>,
    pub sort_order: Option<i32>,
}
pub async fn update_community_rule(
    Path((slug, rule_id)): Path<(String, i64)>,
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<UpdateCommunityRuleForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let title = truncate_chars(payload.title.trim(), MAX_COMMUNITY_RULE_TITLE_CHARS);
    if title.trim().is_empty() {
        return Redirect::to(&format!(
            "/community/{}/settings?status=rule_title_required",
            existing.slug
        ))
        .into_response();
    }
    let body = truncate_chars(
        payload.body.unwrap_or_default().trim(),
        MAX_COMMUNITY_RULE_BODY_CHARS,
    );
    let sort_order = payload.sort_order.unwrap_or(0).max(0);

    match sqlx::query(
        r#"
        UPDATE community_rule
        SET title = $1,
            body = $2,
            sort_order = $3,
            updated_at = CURRENT_TIMESTAMP
        WHERE rule_id = $4
          AND community_id = $5
        "#,
    )
    .bind(title)
    .bind(body)
    .bind(sort_order)
    .bind(rule_id)
    .bind(existing.community_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=rule_updated",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=rule_failed",
            existing.slug
        ))
        .into_response(),
    }
}

pub async fn delete_community_rule(
    Path((slug, rule_id)): Path<(String, i64)>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    match sqlx::query(
        r#"
        DELETE FROM community_rule
        WHERE rule_id = $1
          AND community_id = $2
        "#,
    )
    .bind(rule_id)
    .bind(existing.community_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=rule_deleted",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=rule_failed",
            existing.slug
        ))
        .into_response(),
    }
}


#[derive(Deserialize)]
pub struct AddCommunityModeratorForm {
    pub moderator_lookup: String,
}
pub async fn add_community_moderator(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<AddCommunityModeratorForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let Some(username_lookup) = parse_local_username_lookup(&payload.moderator_lookup) else {
        return Redirect::to(&format!(
            "/community/{}/settings?status=moderator_not_found",
            existing.slug
        ))
        .into_response();
    };

    let target_user_id = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT id
        FROM users
        WHERE LOWER(username) = LOWER($1)
        LIMIT 1
        "#,
    )
    .bind(username_lookup)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(target_user_id) = target_user_id else {
        return Redirect::to(&format!(
            "/community/{}/settings?status=moderator_not_found",
            existing.slug
        ))
        .into_response();
    };

    if target_user_id == existing.owner_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=moderator_already_exists",
            existing.slug
        ))
        .into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO community_member (community_id, user_id, joined_at)
        VALUES ($1, $2, CURRENT_TIMESTAMP)
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .bind(existing.community_id)
    .bind(target_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!(
            "add_community_moderator member insert failed (community_id={}, user_id={}): {}",
            existing.community_id,
            target_user_id,
            err
        );
        return Redirect::to(&format!(
            "/community/{}/settings?status=moderator_add_failed",
            existing.slug
        ))
        .into_response();
    }

    match sqlx::query(
        r#"
        INSERT INTO community_moderator (community_id, user_id, granted_by_user_id, granted_at)
        VALUES ($1, $2, $3, CURRENT_TIMESTAMP)
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .bind(existing.community_id)
    .bind(target_user_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=moderator_added",
            existing.slug
        ))
        .into_response(),
        Ok(_) => Redirect::to(&format!(
            "/community/{}/settings?status=moderator_already_exists",
            existing.slug
        ))
        .into_response(),
        Err(err) => {
            tracing::warn!(
                "add_community_moderator failed (community_id={}, user_id={}): {}",
                existing.community_id,
                target_user_id,
                err
            );
            Redirect::to(&format!(
                "/community/{}/settings?status=moderator_add_failed",
                existing.slug
            ))
            .into_response()
        }
    }
}

pub async fn remove_community_moderator(
    Path((slug, user_id)): Path<(String, i32)>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    if user_id == existing.owner_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=moderator_owner_protected",
            existing.slug
        ))
        .into_response();
    }

    match sqlx::query(
        r#"
        DELETE FROM community_moderator
        WHERE community_id = $1
          AND user_id = $2
        "#,
    )
    .bind(existing.community_id)
    .bind(user_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=moderator_removed",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=moderator_remove_failed",
            existing.slug
        ))
        .into_response(),
    }
}

#[derive(Deserialize)]
pub struct UpdateCommunityForm {
    pub name: String,
    pub description: Option<String>,
}
pub async fn update_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<UpdateCommunityForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let trimmed_name = payload.name.trim();
    if trimmed_name.is_empty() {
        return Redirect::to(&format!(
            "/community/{}/settings?status=name_required",
            existing.slug
        ))
        .into_response();
    }
    let name = truncate_chars(trimmed_name, MAX_COMMUNITY_NAME_CHARS);
    if name.trim().is_empty() {
        return Redirect::to(&format!(
            "/community/{}/settings?status=name_required",
            existing.slug
        ))
        .into_response();
    }

    let description = truncate_chars(
        payload.description.unwrap_or_default().trim(),
        MAX_COMMUNITY_DESCRIPTION_CHARS,
    );

    let update_result = sqlx::query(
        r#"
        UPDATE community_page
        SET
            name = $1,
            description = $2,
            updated_at = CURRENT_TIMESTAMP
        WHERE community_id = $3
          AND owner_user_id = $4
          AND LOWER(COALESCE(status, 'active')) NOT IN ('deleted', 'banned')
        "#,
    )
    .bind(name)
    .bind(description)
    .bind(existing.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await;

    match update_result {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=updated",
            existing.slug
        ))
        .into_response(),
        _ => Redirect::to(&format!(
            "/community/{}/settings?status=update_failed",
            existing.slug
        ))
        .into_response(),
    }
}

pub async fn delete_community(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let can_team_moderate = is_acting_as_team_session(&session).await;
    if existing.owner_user_id != current_user_id && !can_team_moderate {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let delete_result = if can_team_moderate {
        sqlx::query(
            r#"
            UPDATE community_page
            SET status = 'deleted', updated_at = CURRENT_TIMESTAMP
            WHERE community_id = $1
            "#,
        )
        .bind(existing.community_id)
        .execute(&pool)
        .await
    } else {
        sqlx::query(
            r#"
            UPDATE community_page
            SET status = 'deleted', updated_at = CURRENT_TIMESTAMP
            WHERE community_id = $1
              AND owner_user_id = $2
            "#,
        )
        .bind(existing.community_id)
        .bind(current_user_id)
        .execute(&pool)
        .await
    };

    match delete_result {
        Ok(result) if result.rows_affected() > 0 => {
            Redirect::to("/communities?create_status=deleted").into_response()
        }
        _ => Redirect::to(&format!(
            "/community/{}/settings?status=delete_failed",
            existing.slug
        ))
        .into_response(),
    }
}

#[derive(Deserialize)]
pub struct CommunityDetailQuery {
    pub status: Option<String>,
    pub sort: Option<String>,
}
pub async fn community_detail(
    Path(slug): Path<String>,
    Query(query): Query<CommunityDetailQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let found_community = load_community_by_slug(&pool, &slug).await;
    let found = found_community.is_some();
    let community = found_community.unwrap_or_else(|| empty_community_page_view(&slug));
    let active_sort = normalize_community_sort(query.sort.as_deref()).to_string();
    let community_rules = if found {
        load_community_rules(&pool, community.community_id).await
    } else {
        Vec::new()
    };
    let community_moderators = if found {
        load_community_moderators(&pool, community.community_id).await
    } else {
        Vec::new()
    };
    let can_team_moderate = found && is_acting_as_team_session(&session).await;
    let can_manage = current_user_id
        .map(|user_id| user_id == community.owner_user_id)
        .unwrap_or(false);
    let community_is_private = community
        .visibility
        .eq_ignore_ascii_case(POST_VISIBILITY_PRIVATE);
    let is_member = if found {
        match current_user_id {
            Some(user_id) if user_id > 0 => {
                can_manage || is_community_member(&pool, community.community_id, user_id).await
            }
            _ => false,
        }
    } else {
        false
    };
    let is_ignored = if found {
        match current_user_id {
            Some(user_id) if user_id > 0 => {
                is_community_ignored(&pool, community.community_id, user_id).await
            }
            _ => false,
        }
    } else {
        false
    };
    let can_quit = found && is_member && !can_manage;
    let can_join = found && current_user_id.is_some() && !is_member;
    let can_view_posts = found && (!community_is_private || is_member || can_manage);
    let can_create_post = found && (is_member || can_manage);
    let posts = if can_view_posts {
        load_community_posts(&pool, community.community_id, current_user_id, &active_sort).await
    } else {
        Vec::new()
    };

    let template = CommunityTemplate {
        title: "Community".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        active_sort,
        community_rules,
        community_moderators,
        found,
        can_manage,
        can_team_moderate,
        is_member,
        can_quit,
        can_join,
        is_ignored,
        can_view_posts,
        can_create_post,
        posts,
        community,
    };

    if !found {
        return (StatusCode::NOT_FOUND, render_template_response(&template)).into_response();
    }

    render_template_response(&template)
}

#[derive(Template)]
#[template(path = "models/community-settings.html")]
#[allow(dead_code)]
pub struct CommunitySettingsTemplate {
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
    pub found: bool,
    pub can_manage: bool,
    pub manage_message: String,
    pub manage_success: bool,
    pub uploaded_profile_images: Vec<String>,
    pub community_rules: Vec<CommunityRuleView>,
    pub community_moderators: Vec<CommunityModeratorView>,
    pub moderator_candidates: Vec<CommunityModeratorView>,
    pub community: CommunityPageView,
}
pub async fn community_settings(
    Path(slug): Path<String>,
    Query(query): Query<CommunityDetailQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let found_community = load_community_by_slug(&pool, &slug).await;
    let found = found_community.is_some();
    let community = found_community.unwrap_or_else(|| empty_community_page_view(&slug));
    let can_manage = current_user_id
        .map(|user_id| user_id == community.owner_user_id)
        .unwrap_or(false);
    let (manage_message, manage_success) = community_manage_feedback(query.status.as_deref());
    let uploaded_profile_images = if can_manage {
        list_uploaded_images(&pool, community.owner_user_id, "profile").await
    } else {
        Vec::new()
    };
    let community_rules = if found {
        load_community_rules(&pool, community.community_id).await
    } else {
        Vec::new()
    };
    let community_moderators = if found {
        load_community_moderators(&pool, community.community_id).await
    } else {
        Vec::new()
    };
    let moderator_candidates = if can_manage {
        load_community_moderator_candidates(&pool, community.community_id).await
    } else {
        Vec::new()
    };

    let template = CommunitySettingsTemplate {
        title: "Community Settings".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        unread_notifications_count,
        notifications,
        found,
        can_manage,
        manage_message,
        manage_success,
        uploaded_profile_images,
        community_rules,
        community_moderators,
        moderator_candidates,
        community,
    };

    if !found || !can_manage {
        return StatusCode::NOT_FOUND.into_response();
    }

    render_template_response(&template)
}


pub async fn community_upload_photo(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    multipart: Multipart,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let (file_name, content_type, bytes) = match read_uploaded_image(multipart).await {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!("community_upload_photo invalid upload payload: {}", err);
            return Redirect::to(&format!(
                "/community/{}/settings?status=photo_upload_failed",
                existing.slug
            ))
            .into_response();
        }
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
        Err(err) => {
            tracing::warn!("community_upload_photo save failed: {}", err);
            return Redirect::to(&format!(
                "/community/{}/settings?status=photo_upload_failed",
                existing.slug
            ))
            .into_response();
        }
    };

    if let Err(err) =
        store_uploaded_image_record(&pool, current_user_id, "profile", &file_url).await
    {
        tracing::warn!("community_upload_photo record failed: {}", err);
        return Redirect::to(&format!(
            "/community/{}/settings?status=photo_upload_failed",
            existing.slug
        ))
        .into_response();
    }

    match sqlx::query(
        r#"
        UPDATE community_page
        SET profile_photo_url = $1,
            profile_photo_style = '',
            updated_at = CURRENT_TIMESTAMP
        WHERE community_id = $2
          AND owner_user_id = $3
        "#,
    )
    .bind(&file_url)
    .bind(existing.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=photo_uploaded",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=photo_upload_failed",
            existing.slug
        ))
        .into_response(),
    }
}

pub async fn community_select_photo(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
    Form(form): Form<SelectUploadedImageForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    let selected_url = form.file_url.trim();
    if selected_url.is_empty()
        || !is_owned_uploaded_image(&pool, current_user_id, "profile", selected_url).await
    {
        return Redirect::to(&format!(
            "/community/{}/settings?status=image_not_owned",
            existing.slug
        ))
        .into_response();
    }

    let crop_style = crop_style_from_form(&form);
    match sqlx::query(
        r#"
        UPDATE community_page
        SET profile_photo_url = $1,
            profile_photo_style = $2,
            updated_at = CURRENT_TIMESTAMP
        WHERE community_id = $3
          AND owner_user_id = $4
        "#,
    )
    .bind(selected_url)
    .bind(&crop_style)
    .bind(existing.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=photo_selected",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=photo_select_failed",
            existing.slug
        ))
        .into_response(),
    }
}

pub async fn community_reset_photo(
    Path(slug): Path<String>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(existing) = load_community_by_slug(&pool, &slug).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if existing.owner_user_id != current_user_id {
        return Redirect::to(&format!(
            "/community/{}/settings?status=forbidden",
            existing.slug
        ))
        .into_response();
    }

    match sqlx::query(
        r#"
        UPDATE community_page
        SET profile_photo_url = $1,
            profile_photo_style = '',
            updated_at = CURRENT_TIMESTAMP
        WHERE community_id = $2
          AND owner_user_id = $3
        "#,
    )
    .bind(DEFAULT_PROFILE_PHOTO_URL)
    .bind(existing.community_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        Ok(result) if result.rows_affected() > 0 => Redirect::to(&format!(
            "/community/{}/settings?status=photo_reset",
            existing.slug
        ))
        .into_response(),
        Ok(_) | Err(_) => Redirect::to(&format!(
            "/community/{}/settings?status=photo_reset_failed",
            existing.slug
        ))
        .into_response(),
    }
}



/*  -----------------------------------------------------
    |                                                   |
    | Moderator section                                 |
    |                                                   |
    -----------------------------------------------------
*/
#[derive(sqlx::FromRow)]
struct CommunityModeratorRow {
    user_id: i32,
    username: String,
    preferred_username: String,
    profile_photo_url: String,
    profile_photo_style: String,
    is_owner: bool,
}
pub struct CommunityModeratorView {
    pub user_id: i32,
    pub username: String,
    pub preferred_username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub is_owner: bool,
}



/*  -----------------------------------------------------
    |                                                   |
    | Loading section                                   |
    |                                                   |
    -----------------------------------------------------
*/

fn map_community_rule_row_to_view(row: CommunityRuleRow) -> CommunityRuleView {
    CommunityRuleView {
        rule_id: row.rule_id,
        title: row.title,
        body: row.body,
        sort_order: row.sort_order,
    }
}


fn map_community_moderator_row_to_view(row: CommunityModeratorRow) -> CommunityModeratorView {
    CommunityModeratorView {
        user_id: row.user_id,
        username: row.username,
        preferred_username: row.preferred_username,
        profile_photo_url: row.profile_photo_url,
        profile_photo_style: row.profile_photo_style,
        is_owner: row.is_owner,
    }
}


async fn load_community_rules(pool: &PgPool, community_id: i64) -> Vec<CommunityRuleView> {
    if community_id <= 0 {
        return Vec::new();
    }

    let rows = sqlx::query_as::<_, CommunityRuleRow>(
        r#"
        SELECT
            rule_id,
            COALESCE(NULLIF(title, ''), '') AS title,
            COALESCE(NULLIF(body, ''), '') AS body,
            COALESCE(sort_order, 0)::INTEGER AS sort_order
        FROM community_rule
        WHERE community_id = $1
        ORDER BY sort_order ASC, rule_id ASC
        "#,
    )
    .bind(community_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(map_community_rule_row_to_view)
        .collect()
}


async fn load_community_moderators(
    pool: &PgPool,
    community_id: i64,
) -> Vec<CommunityModeratorView> {
    if community_id <= 0 {
        return Vec::new();
    }

    let rows = sqlx::query_as::<_, CommunityModeratorRow>(
        r#"
        SELECT
            u.id AS user_id,
            COALESCE(NULLIF(u.username, ''), '') AS username,
            COALESCE(NULLIF(u.preferred_username, ''), '') AS preferred_username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS profile_photo_style,
            (u.id = c.owner_user_id) AS is_owner
        FROM community_moderator cm
        JOIN users u ON u.id = cm.user_id
        JOIN community_page c ON c.community_id = cm.community_id
        WHERE cm.community_id = $1
        ORDER BY (u.id <> c.owner_user_id) ASC, cm.granted_at ASC, u.id ASC
        "#,
    )
    .bind(community_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(map_community_moderator_row_to_view)
        .collect()
}

async fn load_community_moderator_candidates(
    pool: &PgPool,
    community_id: i64,
) -> Vec<CommunityModeratorView> {
    if community_id <= 0 {
        return Vec::new();
    }

    let rows = sqlx::query_as::<_, CommunityModeratorRow>(
        r#"
        SELECT
            u.id AS user_id,
            COALESCE(NULLIF(u.username, ''), '') AS username,
            COALESCE(NULLIF(u.preferred_username, ''), '') AS preferred_username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS profile_photo_style,
            FALSE AS is_owner
        FROM community_member cm
        JOIN users u ON u.id = cm.user_id
        JOIN community_page c ON c.community_id = cm.community_id
        LEFT JOIN community_moderator mod
               ON mod.community_id = cm.community_id
              AND mod.user_id = cm.user_id
        WHERE cm.community_id = $1
          AND mod.user_id IS NULL
          AND (c.owner_user_id IS NULL OR u.id <> c.owner_user_id)
        ORDER BY cm.joined_at DESC, u.id DESC
        LIMIT 30
        "#,
    )
    .bind(community_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(map_community_moderator_row_to_view)
        .collect()
}


async fn is_community_ignored(pool: &PgPool, community_id: i64, user_id: i32) -> bool {
    if community_id <= 0 || user_id <= 0 {
        return false;
    }

    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM community_ignore ci
            WHERE ci.community_id = $1
              AND ci.user_id = $2
        )
        "#,
    )
    .bind(community_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}


async fn is_community_member(pool: &PgPool, community_id: i64, user_id: i32) -> bool {
    if community_id <= 0 || user_id <= 0 {
        return false;
    }

    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM community_member cm
            WHERE cm.community_id = $1
              AND cm.user_id = $2
        )
        "#,
    )
    .bind(community_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}


fn community_manage_feedback(status: Option<&str>) -> (String, bool) {
    match status.unwrap_or_default() {
        "created" => ("Community page created.".to_string(), true),
        "updated" => ("Community updated.".to_string(), true),
        "photo_uploaded" => ("Community photo uploaded.".to_string(), true),
        "photo_selected" => ("Community photo updated.".to_string(), true),
        "photo_reset" => ("Community photo reset to default.".to_string(), true),
        "rule_added" => ("Community rule added.".to_string(), true),
        "rule_updated" => ("Community rule updated.".to_string(), true),
        "rule_deleted" => ("Community rule deleted.".to_string(), true),
        "moderator_added" => ("Community moderator added.".to_string(), true),
        "moderator_removed" => ("Community moderator removed.".to_string(), true),
        "deleted" => ("Community deleted.".to_string(), true),
        "forbidden" => (
            "Only the creator can manage this community.".to_string(),
            false,
        ),
        "name_required" => ("Community name is required.".to_string(), false),
        "rule_title_required" => ("Rule title is required.".to_string(), false),
        "rule_failed" => (
            "Unable to update community rules right now. Please retry.".to_string(),
            false,
        ),
        "moderator_not_found" => ("Moderator user was not found.".to_string(), false),
        "moderator_already_exists" => ("User is already a moderator.".to_string(), false),
        "moderator_add_failed" => (
            "Unable to add moderator right now. Please retry.".to_string(),
            false,
        ),
        "moderator_remove_failed" => (
            "Unable to remove moderator right now. Please retry.".to_string(),
            false,
        ),
        "moderator_owner_protected" => (
            "Community creator cannot be removed from moderators.".to_string(),
            false,
        ),
        "image_not_owned" => ("Selected image is not available.".to_string(), false),
        "update_failed" => (
            "Unable to update community right now. Please retry.".to_string(),
            false,
        ),
        "photo_upload_failed" => (
            "Unable to upload community photo right now. Please retry.".to_string(),
            false,
        ),
        "photo_select_failed" => (
            "Unable to update community photo right now. Please retry.".to_string(),
            false,
        ),
        "photo_reset_failed" => (
            "Unable to reset community photo right now. Please retry.".to_string(),
            false,
        ),
        "delete_failed" => (
            "Unable to delete community right now. Please retry.".to_string(),
            false,
        ),
        _ => (String::new(), false),
    }
}


async fn allocate_unique_community_slug(
    pool: &PgPool,
    requested_slug: &str,
    fallback_name: &str,
) -> Option<String> {
    let mut base_slug = slugify_community_value(requested_slug);
    if base_slug.is_empty() {
        base_slug = slugify_community_value(fallback_name);
    }
    if base_slug.is_empty() {
        return None;
    }

    let exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM community_page
            WHERE LOWER(COALESCE(slug, '')) = LOWER($1)
        )
        "#,
    )
    .bind(&base_slug)
    .fetch_one(pool)
    .await
    .unwrap_or(true);
    if !exists {
        return Some(base_slug);
    }

    for suffix in 2..=9_999 {
        let candidate = format!("{}-{}", base_slug, suffix);
        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM community_page
                WHERE LOWER(COALESCE(slug, '')) = LOWER($1)
            )
            "#,
        )
        .bind(&candidate)
        .fetch_one(pool)
        .await
        .unwrap_or(true);

        if !exists {
            return Some(candidate);
        }
    }

    None
}


fn normalize_community_sort(raw: Option<&str>) -> &'static str {
    match raw
        .map(str::trim)
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("new") => "new",
        Some("top") => "top",
        _ => "hot",
    }
}

fn normalize_community_slug(raw: &str) -> String {
    raw.trim().trim_matches('/').to_string()
}

fn slugify_community_value(raw: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            previous_was_dash = false;
        } else if matches!(lower, '-' | '_' | ' ' | '.') {
            if !slug.is_empty() && !previous_was_dash {
                slug.push('-');
                previous_was_dash = true;
            }
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    slug.chars().take(MAX_COMMUNITY_SLUG_CHARS).collect()
}