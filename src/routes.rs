use askama::Template;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use sqlx::PgPool;
use tower_sessions::Session;

use crate::handlers::notifier::{HeaderNotificationView, load_header_notifications};
use crate::handlers::post::{IndexPostView, load_index_posts};
use crate::handlers::user::{load_is_moderator, session_public_user_id, session_user_id};


pub fn render_template_response(template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!("template render failed: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Template)]
#[template(path = "main/index.html")]
#[allow(dead_code)]
pub struct IndexTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub posts: Vec<IndexPostView>,
    pub can_create_post: bool,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    username: String,
}


// Service state, route response

// Response : HTTP/1.1 200 OK   Content-Type: application/json  { "status": "ok" }
async fn health_live() -> impl IntoResponse {
    (
        StatusCode::OK,                            // HTTP code 200
        Json(serde_json::json!({"status": "ok"})), // JSON Response code 200 : { "status": "ok" }
    )
}

// Verify database connection
async fn health_ready(
    axum::extract::State(pool): axum::extract::State<sqlx::PgPool>,
) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool).await // Connect to Database, "_" : Detect type, i32 : 32 bit
    {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({
            "status": "ready"
            })),).into_response(),
        Err(err) => {
            tracing::warn!("health readiness check failed: {}", err);(StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                "status": "unavailable"
                })),).into_response()
        }
    }
}


pub fn redirect_back_path(headers: &HeaderMap) -> String {
    headers
        .get(REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split("://").nth(1).map(|tail| tail.to_string()))
        .and_then(|value| value.find('/').map(|idx| value[idx..].to_string()))
        .filter(|value| value.starts_with('/') && !value.starts_with("//"))
        .unwrap_or_else(|| "/".to_string())
}


pub async fn index(session: Session, State(pool): State<PgPool>) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let posts = load_index_posts(&pool, current_user_id).await;
    let template = IndexTemplate {
        title: "Feeds".to_string(),
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        posts,
        can_create_post: current_user_id_value != 0,
        unread_notifications_count,
        notifications,
        username: session_string(&session, "username", "").await,
    };
    render_template_response(&template)
}

async fn login() -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

async fn register() -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

pub fn routes() -> Router<sqlx::PgPool> {
    Router::new()
        .route("/", get(index))
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/login", get(login))
        .route("/register", get(register))
}

pub async fn terms_page(session: Session, State(pool): State<PgPool>) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let template = PolicyPageTemplate {
        title: "Terms and Conditions - Instavox".to_string(),
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
        page_heading: "Terms and Conditions".to_string(),
        page_content: "By creating an account, you agree to use Instavox lawfully, avoid harassment, abusive behavior, spam, and illegal content, and respect moderator decisions. You remain responsible for content posted from your account. Accounts may be restricted or removed for policy violations.".to_string(),
    };

    render_template_response(&template)
}

#[derive(Template)]
#[template(path = "main/policy_page.html")]
#[allow(dead_code)]
pub struct PolicyPageTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    pub username: String,
    pub page_heading: String,
    pub page_content: String,
}

pub async fn privacy_policy_page(
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let template = PolicyPageTemplate {
        title: "Privacy Policy - Instavox".to_string(),
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
        page_heading: "Privacy Policy".to_string(),
        page_content: "Instavox stores account information, profile details, posts, messages, and moderation records needed to operate the service. Your data is used for account access, social features, safety moderation, and federation when enabled. Do not share confidential information in public posts. You can request account deletion from Settings.".to_string(),
    };

    render_template_response(&template)
}


pub fn empty_index_post_view(post_id: i64) -> IndexPostView {
    IndexPostView {
        post_id,
        author_public_id: 0,
        author_username: String::new(),
        author_profile_photo_url: "/public/avatar.webp".to_string(),
        author_profile_photo_style: String::new(),
        body: String::new(),
        link_url: String::new(),
        visibility: POST_VISIBILITY_PUBLIC.to_string(),
        visibility_label: "Public".to_string(),
        community_name: String::new(),
        community_slug: String::new(),
        link_title: String::new(),
        link_description: String::new(),
        link_image_url: String::new(),
        has_link_preview: false,
        image_urls: Vec::new(),
        likes_count: 0,
        dislikes_count: 0,
        comments_count: 0,
        shares_count: 0,
        liked_by_current_user: false,
        disliked_by_current_user: false,
        shared_by_current_user: false,
        comments: Vec::new(),
        created_at: String::new(),
    }
}

pub fn empty_community_page_view(slug: &str) -> CommunityPageView {
    CommunityPageView {
        community_id: 0,
        slug: normalize_community_slug(slug),
        name: String::new(),
        description: String::new(),
        visibility: POST_VISIBILITY_PUBLIC.to_string(),
        member_count: 0,
        post_count: 0,
        profile_photo_url: "/public/avatar.webp".to_string(),
        profile_photo_style: String::new(),
        owner_user_id: 0,
        owner_username: String::new(),
        created_at: String::new(),
    }
}