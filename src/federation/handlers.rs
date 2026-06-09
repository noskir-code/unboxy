use activitypub_federation::{
    FEDERATION_CONTENT_TYPE,
    activity_sending::SendActivityTask,
    config::{Data, FederationConfig, UrlVerifier},
    error::Error as ApError,
    fetch::{object_id::ObjectId, webfinger::build_webfinger_response},
    http_signatures::generate_actor_keypair,
    kinds::{
        activity::{AcceptType, CreateType, FollowType, UndoType, UpdateType},
        actor::PersonType,
        object::NoteType,
        public,
    },
    protocol::{context::WithContext, public_key::PublicKey, verification::verify_domains_match},
    traits::{ActivityHandler, Actor, Object},
};
use async_trait::async_trait;
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Path, Query, State},
    http::{
        HeaderMap, StatusCode,
        header::{ACCEPT, CONTENT_TYPE, LOCATION},
    },
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;const DEFAULT_PROFILE_PHOTO_URL: &str = "/public/avatar.webp";
const DEFAULT_BACKGROUND_PHOTO_URL: &str = "/public/pexels-enginakyurt-17902901.webp";
use sqlx::PgPool;
use std::{borrow::Cow, sync::OnceLock};
use url::Url;

const CANONICAL_INSTAVOX_DOMAIN: &str = "instavox.social";
const DEFAULT_PROFILE_PHOTO_URL: &str = "/public/avatar.webp";
const DEFAULT_BACKGROUND_PHOTO_URL: &str = "/public/pexels-enginakyurt-17902901.webp";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPubImage {
    #[serde(rename = "type")]
    kind: Cow<'static, str>,
    media_type: Cow<'static, str>,
    url: Url,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPubEndpoints {
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_inbox: Option<Url>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPubAttachment {
    #[serde(rename = "type")]
    kind: Cow<'static, str>,
    media_type: String,
    url: Url,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPubNote {
    id: Url,
    #[serde(rename = "type")]
    kind: NoteType,
    attributed_to: Url,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to: Vec<Url>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cc: Vec<Url>,
    content: String,
    url: Url,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachment: Vec<ActivityPubAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderedCollection {
    id: Url,
    #[serde(rename = "type")]
    kind: Cow<'static, str>,
    total_items: usize,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    ordered_items: Vec<Url>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UndoObject {
    Follow(Box<FollowActivity>),
    Id(Url),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: UndoType,
    actor: ObjectId<FederatedPersonActor>,
    object: UndoObject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: AcceptType,
    actor: ObjectId<FederatedPersonActor>,
    object: FollowActivity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PersonAcceptedActivities {
    Create(CreatePostActivity),
    UpdatePerson(UpdatePersonActivity),
    UpdatePost(UpdatePostActivity),
    Follow(FollowActivity),
    Undo(UndoActivity),
}

#[derive(Deserialize)]
pub struct WebfingerQuery {
    pub resource: String,
}

fn request_scheme(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "http".to_string())
}

fn remote_search_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent("Instavox/0.1 (+https://instavox.social)")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn prefers_browser_html(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(ACCEPT).and_then(|value| value.to_str().ok()) else {
        return true;
    };

    let normalized = accept.to_ascii_lowercase();
    let wants_activitypub = normalized.contains("application/activity+json")
        || normalized.contains("application/ld+json")
        || normalized.contains("application/jrd+json");
    let wants_html = normalized.contains("text/html")
        || normalized.contains("application/xhtml+xml")
        || normalized.contains("*/*");

    wants_html && !wants_activitypub
}

fn parse_webfinger_resource(resource: &str) -> Option<(String, String)> {
    let value = resource.trim();
    let value = value.strip_prefix("acct:").unwrap_or(value);
    let value = value.strip_prefix('@').unwrap_or(value);
    let (username, domain) = value.split_once('@')?;
    if username.trim().is_empty() || domain.trim().is_empty() {
        return None;
    }
    Some((username.trim().to_string(), domain.trim().to_string()))
}

fn parse_remote_search_handle(search_term: &str) -> Option<(String, String)> {
    let (username, domain) = parse_webfinger_resource(search_term)?;
    if domain.eq_ignore_ascii_case(CANONICAL_INSTAVOX_DOMAIN) {
        return None;
    }
    Some((username, domain))
}

fn host_matches_resource(host: &str, resource_domain: &str) -> bool {
    let host_lower = host.to_ascii_lowercase();
    let domain_lower = resource_domain.to_ascii_lowercase();
    if host_lower == domain_lower {
        return true;
    }

    let host_without_port = host_lower
        .split_once(':')
        .map(|(value, _)| value)
        .unwrap_or(host_lower.as_str());
    let domain_without_port = domain_lower
        .split_once(':')
        .map(|(value, _)| value)
        .unwrap_or(domain_lower.as_str());
    host_without_port == domain_without_port
}

fn url_host_with_port(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => CANONICAL_INSTAVOX_DOMAIN.to_string(),
    }
}


fn absolutize_media_url(base_url: &str, value: Option<&str>) -> Option<Url> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(raw) {
        return Some(url);
    }
    let root = Url::parse(base_url).ok()?;
    root.join(raw).ok()
}

fn non_default_local_media_url(
    base_url: &str,
    value: Option<&str>,
    default_value: &str,
) -> Option<Url> {
    let raw = value?.trim();
    if raw.is_empty() || raw == default_value {
        return None;
    }
    absolutize_media_url(base_url, Some(raw))
}

fn guess_media_type(url: &Url) -> &'static str {
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".avif") {
        "image/avif"
    } else {
        "image/jpeg"
    }
}

fn escape_html_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn strip_html_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_search_preview(value: &str, max_chars: usize) -> String {
    let clean = strip_html_tags(value);
    if clean.chars().count() <= max_chars {
        return clean;
    }
    let mut preview: String = clean.chars().take(max_chars).collect();
    preview.push_str("...");
    preview
}

fn is_public_recipient(urls: &[Url]) -> bool {
    urls.iter()
        .any(|url| url.as_str() == "https://www.w3.org/ns/activitystreams#Public")
}

fn activity_or_object_is_public(
    activity_to: &[Url],
    activity_cc: &[Url],
    object_to: &[Url],
    object_cc: &[Url],
) -> bool {
    is_public_recipient(activity_to)
        || is_public_recipient(activity_cc)
        || is_public_recipient(object_to)
        || is_public_recipient(object_cc)
}



fn post_note_url(base_url: &str, post_id: i64) -> Result<Url, ApError> {
    Url::parse(&format!("{base_url}/ap/posts/{post_id}"))
        .map_err(|err| federation_error(format!("invalid post activitypub url: {err}")))
}

fn public_post_url(base_url: &str, post_id: i64) -> Result<Url, ApError> {
    Url::parse(&format!("{base_url}/posts/{post_id}"))
        .map_err(|err| federation_error(format!("invalid public post url: {err}")))
}



fn note_recipients(actor_id: &Url, visibility: &str) -> Result<(Vec<Url>, Vec<Url>), ApError> {
    let followers = followers_url_from_actor(actor_id)?;
    if visibility.trim().eq_ignore_ascii_case("public") {
        Ok((vec![public()], vec![followers]))
    } else {
        Ok((vec![followers], Vec::new()))
    }
}

fn render_note_content(post: &LocalPostRow) -> String {
    let mut parts = Vec::new();
    let body = post.body.trim();
    if !body.is_empty() {
        parts.push(format!(
            "<p>{}</p>",
            escape_html_text(body).replace('\n', "<br>")
        ));
    }

    let link_url = post.link_url.trim();
    if !link_url.is_empty() {
        let safe_link = escape_html_text(link_url);
        parts.push(format!(
            r#"<p><a href="{safe_link}" rel="nofollow noopener noreferrer">{safe_link}</a></p>"#
        ));
    }

    parts.join("")
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nested_json_string<'a>(value: &'a Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn extract_remote_icon_url(value: &Value) -> String {
    if let Some(icon_value) = value.get("icon") {
        if let Some(url) = icon_value.as_str() {
            return url.trim().to_string();
        }
        if let Some(url) = icon_value.get("url").and_then(Value::as_str) {
            return url.trim().to_string();
        }
        if let Some(items) = icon_value.get("url").and_then(Value::as_array) {
            for item in items {
                if let Some(url) = item.as_str() {
                    return url.trim().to_string();
                }
                if let Some(url) = item.get("href").and_then(Value::as_str) {
                    return url.trim().to_string();
                }
            }
        }
    }
    String::new()
}

fn federation_error(message: impl Into<String>) -> ApError {
    ApError::Other(message.into())
}

fn normalize_discovery_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "discover" | "discovered" | "authorized" | "allow" | "allowed" => "discovered".to_string(),
        "limit" | "limited" => "limited".to_string(),
        "ban" | "banned" | "blocked" | "block" => "ban".to_string(),
        _ => "discovered".to_string(),
    }
}

#[derive(Clone)]
struct InstanceUrlVerifier {
    pool: PgPool,
}

#[async_trait]
impl UrlVerifier for InstanceUrlVerifier {
    async fn verify(&self, url: &Url) -> Result<(), activitypub_federation::error::Error> {
        let Some(host) = url
            .host_str()
            .map(|value| value.trim().to_ascii_lowercase())
        else {
            return Ok(());
        };
        if host.is_empty() || host == CANONICAL_INSTAVOX_DOMAIN {
            return Ok(());
        }

        let status = get_instance_status(&self.pool, &host)
            .await
            .map_err(|err| federation_error(format!("instance status check failed: {err}")))?;
        if status == "limited" || status == "ban" {
            return Err(federation_error(format!("instance {host} is restricted")));
        }
        Ok(())
    }
}


async fn get_remote_user_status(pool: &PgPool, actor_id: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT LOWER(COALESCE(NULLIF(status, ''), 'discovered'))
        FROM ap_remote_actor
        WHERE actor_id = $1
        LIMIT 1
        "#,
    )
    .bind(actor_id)
    .fetch_optional(pool)
    .await
    .map(|value| normalize_discovery_status(&value.unwrap_or_else(|| "discovered".to_string())))
}

async fn federation_request_data(
    pool: &PgPool,
    domain: &str,
    allow_http: bool,
) -> Result<Data<PgPool>, ApError> {
    let mut builder = FederationConfig::builder();
    builder
        .domain(domain.to_string())
        .app_data(pool.clone())
        .debug(allow_http)
        .allow_http_urls(allow_http)
        .url_verifier(Box::new(InstanceUrlVerifier { pool: pool.clone() }));
    let config = builder
        .build()
        .await
        .map_err(|err| federation_error(format!("failed to build federation config: {err}")))?;
    Ok(config.to_request_data())
}

async fn ensure_user_activitypub_data(
    pool: &PgPool,
    user_id: i32,
    preferred_username: &str,
    base_url: &str,
    ap_id: Option<&str>,
    ap_inbox: Option<&str>,
    ap_outbox: Option<&str>,
    ap_public_key: Option<&str>,
    ap_private_key: Option<&str>,
) -> Result<UserActivityPubData, String> {
    let has_public = ap_public_key.is_some_and(|value| !value.trim().is_empty());
    let has_private = ap_private_key.is_some_and(|value| !value.trim().is_empty());
    let has_ap_id = ap_id.is_some_and(|value| !value.trim().is_empty());
    let has_ap_inbox = ap_inbox.is_some_and(|value| !value.trim().is_empty());
    let has_ap_outbox = ap_outbox.is_some_and(|value| !value.trim().is_empty());

    let (public_key_pem, private_key_pem) = if has_public && has_private {
        (
            ap_public_key.unwrap_or_default().to_string(),
            ap_private_key.unwrap_or_default().to_string(),
        )
    } else {
        let keypair = generate_actor_keypair().map_err(|err| err.to_string())?;
        (keypair.public_key, keypair.private_key)
    };

    let actor_data = if has_ap_id && has_ap_inbox && has_ap_outbox {
        let mut generated = build_actor_url_strings(base_url, preferred_username);
        generated.actor_id = ap_id.unwrap_or_default().to_string();
        generated.inbox = ap_inbox.unwrap_or_default().to_string();
        generated.outbox = ap_outbox.unwrap_or_default().to_string();
        generated.public_key_pem = public_key_pem.clone();
        generated.private_key_pem = Some(private_key_pem.clone());
        generated
    } else {
        let mut generated = build_actor_url_strings(base_url, preferred_username);
        generated.public_key_pem = public_key_pem.clone();
        generated.private_key_pem = Some(private_key_pem.clone());
        generated
    };

    sqlx::query(
        r#"
        UPDATE users
        SET ap_public_key = $1,
            ap_private_key = $2,
            ap_id = $3,
            ap_inbox = $4,
            ap_outbox = $5,
            ap_local = TRUE,
            ap_last_refreshed_at = COALESCE(ap_last_refreshed_at, CURRENT_TIMESTAMP)
        WHERE id = $6
        "#,
    )
    .bind(&public_key_pem)
    .bind(&private_key_pem)
    .bind(&actor_data.actor_id)
    .bind(&actor_data.inbox)
    .bind(&actor_data.outbox)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|err| err.to_string())?;

    Ok(actor_data)
}

async fn load_local_actor_by_identifier(
    pool: &PgPool,
    requested_username: &str,
) -> Result<Option<LocalActorRow>, ApError> {
    sqlx::query_as::<_, LocalActorRow>(
        r#"
        SELECT
            id AS user_id,
            preferred_username,
            COALESCE(NULLIF(federation_display_name_mode, ''), 'full_name') AS federation_display_name_mode,
            first_name,
            COALESCE(first_name_public, TRUE) AS first_name_public,
            last_name,
            COALESCE(last_name_public, TRUE) AS last_name_public,
            bio_description,
            profile_photo_url,
            background_photo_url,
            created_at,
            ap_id,
            ap_inbox,
            ap_outbox,
            ap_public_key,
            ap_private_key
        FROM users
        WHERE (
            LOWER(preferred_username) = LOWER($1)
            OR LOWER(username) = LOWER($1)
        )
          AND COALESCE(federation_enabled, TRUE) = TRUE
        ORDER BY CASE
            WHEN LOWER(preferred_username) = LOWER($1) THEN 0
            ELSE 1
        END
        LIMIT 1
        "#,
    )
    .bind(requested_username)
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local actor: {err}")))
}

async fn load_local_actor_by_ap_id(
    pool: &PgPool,
    actor_id: &Url,
) -> Result<Option<LocalActorRow>, ApError> {
    sqlx::query_as::<_, LocalActorRow>(
        r#"
        SELECT
            id AS user_id,
            preferred_username,
            COALESCE(NULLIF(federation_display_name_mode, ''), 'full_name') AS federation_display_name_mode,
            first_name,
            COALESCE(first_name_public, TRUE) AS first_name_public,
            last_name,
            COALESCE(last_name_public, TRUE) AS last_name_public,
            bio_description,
            profile_photo_url,
            background_photo_url,
            created_at,
            ap_id,
            ap_inbox,
            ap_outbox,
            ap_public_key,
            ap_private_key
        FROM users
        WHERE ap_id = $1
          AND COALESCE(federation_enabled, TRUE) = TRUE
        LIMIT 1
        "#,
    )
    .bind(actor_id.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local actor by ap id: {err}")))
}

async fn load_local_actor_by_user_id(
    pool: &PgPool,
    user_id: i32,
) -> Result<Option<LocalActorRow>, ApError> {
    sqlx::query_as::<_, LocalActorRow>(
        r#"
        SELECT
            id AS user_id,
            preferred_username,
            COALESCE(NULLIF(federation_display_name_mode, ''), 'full_name') AS federation_display_name_mode,
            first_name,
            COALESCE(first_name_public, TRUE) AS first_name_public,
            last_name,
            COALESCE(last_name_public, TRUE) AS last_name_public,
            bio_description,
            profile_photo_url,
            background_photo_url,
            created_at,
            ap_id,
            ap_inbox,
            ap_outbox,
            ap_public_key,
            ap_private_key
        FROM users
        WHERE id = $1
          AND COALESCE(federation_enabled, TRUE) = TRUE
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local actor by user id: {err}")))
}

async fn local_actor_from_row(
    pool: &PgPool,
    row: LocalActorRow,
    base_url: &str,
) -> Result<FederatedPersonActor, ApError> {
    let actor_data = ensure_user_activitypub_data(
        pool,
        row.user_id,
        &row.preferred_username,
        base_url,
        row.ap_id.as_deref(),
        row.ap_inbox.as_deref(),
        row.ap_outbox.as_deref(),
        row.ap_public_key.as_deref(),
        row.ap_private_key.as_deref(),
    )
    .await
    .map_err(federation_error)?;

    let actor_id = Url::parse(&actor_data.actor_id)
        .map_err(|err| federation_error(format!("invalid local actor id: {err}")))?;
    let inbox = Url::parse(&actor_data.inbox)
        .map_err(|err| federation_error(format!("invalid local inbox: {err}")))?;
    let outbox = Url::parse(&actor_data.outbox)
        .map_err(|err| federation_error(format!("invalid local outbox: {err}")))?;
    let followers = Url::parse(&actor_data.followers)
        .map_err(|err| federation_error(format!("invalid local followers url: {err}")))?;
    let following = Url::parse(&actor_data.following)
        .map_err(|err| federation_error(format!("invalid local following url: {err}")))?;

    Ok(FederatedPersonActor {
        local_user_id: Some(row.user_id),
        preferred_username: row.preferred_username.clone(),
        display_name: actor_display_name(
            &row.preferred_username,
            &row.federation_display_name_mode,
            &row.first_name,
            row.first_name_public,
            &row.last_name,
            row.last_name_public,
        ),
        summary: row.bio_description.unwrap_or_default().trim().to_string(),
        created_at: row.created_at,
        actor_id,
        inbox,
        outbox: Some(outbox),
        followers: Some(followers),
        following: Some(following),
        public_key_pem: actor_data.public_key_pem,
        private_key_pem: actor_data.private_key_pem,
        shared_inbox: None,
        icon_url: non_default_local_media_url(
            base_url,
            row.profile_photo_url.as_deref(),
            DEFAULT_PROFILE_PHOTO_URL,
        ),
        image_url: non_default_local_media_url(
            base_url,
            row.background_photo_url.as_deref(),
            DEFAULT_BACKGROUND_PHOTO_URL,
        ),
        last_refreshed_at: Some(Utc::now()),
    })
}

async fn load_local_post(pool: &PgPool, post_id: i64) -> Result<Option<LocalPostRow>, ApError> {
    sqlx::query_as::<_, LocalPostRow>(
        r#"
        SELECT
            post_id,
            user_id,
            COALESCE(NULLIF(body, ''), '') AS body,
            COALESCE(NULLIF(link_url, ''), '') AS link_url,
            LOWER(COALESCE(NULLIF(visibility, ''), 'public')) AS visibility,
            community_id,
            created_at
        FROM posts
        WHERE post_id = $1
        LIMIT 1
        "#,
    )
    .bind(post_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local post: {err}")))
}

async fn load_local_post_images(
    pool: &PgPool,
    post_id: i64,
) -> Result<Vec<LocalPostImageRow>, ApError> {
    sqlx::query_as::<_, LocalPostImageRow>(
        r#"
        SELECT
            COALESCE(NULLIF(image_url, ''), '') AS image_url,
            COALESCE(NULLIF(mime_type, ''), 'image/jpeg') AS mime_type
        FROM post_image
        WHERE post_id = $1
        ORDER BY sort_order ASC, image_id ASC
        "#,
    )
    .bind(post_id)
    .fetch_all(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local post images: {err}")))
}

async fn local_post_to_note(
    pool: &PgPool,
    post: &LocalPostRow,
    actor: &FederatedPersonActor,
    base_url: &str,
) -> Result<ActivityPubNote, ApError> {
    let id = post_note_url(base_url, post.post_id)?;
    let url = public_post_url(base_url, post.post_id)?;
    let (to, cc) = note_recipients(&actor.actor_id, &post.visibility)?;
    let images = load_local_post_images(pool, post.post_id).await?;
    let attachment = images
        .into_iter()
        .filter_map(|image| {
            absolutize_media_url(base_url, Some(&image.image_url)).map(|url| {
                ActivityPubAttachment {
                    kind: Cow::Borrowed("Image"),
                    media_type: image.mime_type,
                    url,
                    name: String::new(),
                }
            })
        })
        .collect();

    Ok(ActivityPubNote {
        id,
        kind: Default::default(),
        attributed_to: actor.actor_id.clone(),
        to,
        cc,
        content: render_note_content(post),
        url,
        published: post.created_at,
        attachment,
    })
}

async fn remote_follower_inboxes(pool: &PgPool, local_user_id: i32) -> Result<Vec<Url>, ApError> {
    let rows = sqlx::query_as::<_, RemoteFollowerInboxRow>(
        r#"
        SELECT DISTINCT COALESCE(NULLIF(ra.shared_inbox_url, ''), ra.inbox_url) AS inbox_url
        FROM ap_remote_follow rf
        JOIN ap_remote_actor ra ON ra.actor_id = rf.remote_actor_id
        LEFT JOIN discovered_instance di ON di.host = ra.host
        WHERE rf.local_user_id = $1
          AND rf.status = 'accepted'
          AND COALESCE(NULLIF(ra.inbox_url, ''), '') <> ''
          AND LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) <> 'ban'
          AND LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) <> 'ban'
        ORDER BY inbox_url ASC
        "#,
    )
    .bind(local_user_id)
    .fetch_all(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load remote follower inboxes: {err}")))?;

    Ok(rows
        .into_iter()
        .filter_map(|row| Url::parse(row.inbox_url.trim()).ok())
        .collect())
}

async fn remote_subscribed_post_inboxes(pool: &PgPool) -> Result<Vec<Url>, ApError> {
    let rows = sqlx::query_as::<_, RemoteFollowerInboxRow>(
        r#"
        SELECT DISTINCT COALESCE(NULLIF(ra.shared_inbox_url, ''), ra.inbox_url) AS inbox_url
        FROM ap_remote_actor ra
        LEFT JOIN discovered_instance di ON di.host = COALESCE(NULLIF(ra.host, ''), LOWER(REGEXP_REPLACE(ra.actor_id, '^[a-z]+://([^/]+)/?.*$', '\1')))
        WHERE COALESCE(NULLIF(ra.inbox_url, ''), '') <> ''
          AND LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) = 'discovered'
          AND LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) = 'discovered'
        ORDER BY inbox_url ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|err| {
        federation_error(format!(
            "failed to load subscribed remote post inboxes: {err}"
        ))
    })?;

    Ok(rows
        .into_iter()
        .filter_map(|row| Url::parse(row.inbox_url.trim()).ok())
        .collect())
}

fn remote_actor_from_row(row: RemoteActorRow) -> Result<FederatedPersonActor, ApError> {
    let actor_id = Url::parse(&row.actor_id)
        .map_err(|err| federation_error(format!("invalid remote actor id: {err}")))?;
    let inbox = Url::parse(&row.inbox_url)
        .map_err(|err| federation_error(format!("invalid remote inbox: {err}")))?;
    let outbox = if row.outbox_url.trim().is_empty() {
        None
    } else {
        Some(
            Url::parse(&row.outbox_url)
                .map_err(|err| federation_error(format!("invalid remote outbox: {err}")))?,
        )
    };
    let followers = if row.followers_url.trim().is_empty() {
        None
    } else {
        Some(
            Url::parse(&row.followers_url)
                .map_err(|err| federation_error(format!("invalid remote followers: {err}")))?,
        )
    };
    let following = if row.following_url.trim().is_empty() {
        None
    } else {
        Some(
            Url::parse(&row.following_url)
                .map_err(|err| federation_error(format!("invalid remote following: {err}")))?,
        )
    };
    let shared_inbox = if row.shared_inbox_url.trim().is_empty() {
        None
    } else {
        Some(
            Url::parse(&row.shared_inbox_url)
                .map_err(|err| federation_error(format!("invalid remote shared inbox: {err}")))?,
        )
    };
    let icon_url = if row.icon_url.trim().is_empty() {
        None
    } else {
        Some(
            Url::parse(&row.icon_url)
                .map_err(|err| federation_error(format!("invalid remote icon url: {err}")))?,
        )
    };

    Ok(FederatedPersonActor {
        local_user_id: None,
        preferred_username: row.preferred_username,
        display_name: row.display_name,
        summary: row.summary,
        created_at: None,
        actor_id,
        inbox,
        outbox,
        followers,
        following,
        public_key_pem: row.public_key_pem,
        private_key_pem: None,
        shared_inbox,
        icon_url,
        image_url: None,
        last_refreshed_at: row.last_refreshed_at.or(row.discovered_at),
    })
}

async fn load_remote_actor(
    pool: &PgPool,
    actor_id: &Url,
) -> Result<Option<FederatedPersonActor>, ApError> {
    if let Some(host) = actor_id.host_str() {
        if get_instance_status(pool, host)
            .await
            .map_err(|err| federation_error(format!("failed to check instance status: {err}")))?
            == "ban"
        {
            return Ok(None);
        }
    }

    let row = sqlx::query_as::<_, RemoteActorRow>(
        r#"
        SELECT
            actor_id,
            COALESCE(NULLIF(host, ''), '') AS host,
            preferred_username,
            display_name,
            summary,
            inbox_url,
            shared_inbox_url,
            outbox_url,
            followers_url,
            following_url,
            public_key_pem,
            icon_url,
            LOWER(COALESCE(NULLIF(status, ''), 'discovered')) AS status,
            discovered_at,
            last_refreshed_at
        FROM ap_remote_actor
        WHERE actor_id = $1
        LIMIT 1
        "#,
    )
    .bind(actor_id.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load remote actor: {err}")))?;

    match row {
        Some(row) if normalize_discovery_status(&row.status) == "ban" => Ok(None),
        Some(row) => remote_actor_from_row(row).map(Some),
        None => Ok(None),
    }
}

async fn load_cached_remote_actor_for_search(
    pool: &PgPool,
    username: &str,
    domain: &str,
) -> Result<Option<RemoteActorRow>, ApError> {
    sqlx::query_as::<_, RemoteActorRow>(
        r#"
        SELECT
            actor_id,
            COALESCE(NULLIF(host, ''), '') AS host,
            preferred_username,
            display_name,
            summary,
            inbox_url,
            shared_inbox_url,
            outbox_url,
            followers_url,
            following_url,
            public_key_pem,
            icon_url,
            LOWER(COALESCE(NULLIF(status, ''), 'discovered')) AS status,
            discovered_at,
            last_refreshed_at
        FROM ap_remote_actor
        WHERE LOWER(preferred_username) = LOWER($1)
          AND (
              actor_id ILIKE ('https://' || $2 || '/%')
              OR actor_id ILIKE ('http://' || $2 || '/%')
          )
        ORDER BY last_refreshed_at DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .bind(username)
    .bind(domain)
    .fetch_optional(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load cached remote actor: {err}")))
}

async fn upsert_remote_actor_from_value(
    pool: &PgPool,
    actor_url: &str,
    value: &Value,
) -> Result<RemoteActorRow, ApError> {
    let actor_id = json_string(value, "id").unwrap_or_else(|| actor_url.to_string());
    let actor_host = Url::parse(&actor_id)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()))
        .unwrap_or_default();
    if actor_host.is_empty() {
        return Err(federation_error("remote actor has no valid host"));
    }

    let instance_status = get_instance_status(pool, &actor_host)
        .await
        .map_err(|err| federation_error(format!("failed to load instance status: {err}")))?;
    if instance_status == "limited" || instance_status == "ban" {
        return Err(federation_error(format!(
            "instance {} is restricted",
            actor_host
        )));
    }
    let current_user_status = get_remote_user_status(pool, &actor_id)
        .await
        .map_err(|err| federation_error(format!("failed to load remote user status: {err}")))?;
    if current_user_status == "ban" {
        return Err(federation_error(format!(
            "remote user {actor_id} is banned"
        )));
    }
    let preferred_username = json_string(value, "preferredUsername")
        .or_else(|| json_string(value, "preferred_username"))
        .unwrap_or_default();
    let display_name = json_string(value, "name").unwrap_or_else(|| preferred_username.clone());
    let summary = json_string(value, "summary").unwrap_or_default();
    let inbox_url = json_string(value, "inbox").unwrap_or_default();
    let shared_inbox_url = nested_json_string(value, &["endpoints", "sharedInbox"])
        .or_else(|| nested_json_string(value, &["endpoints", "shared_inbox"]))
        .unwrap_or_default();
    let outbox_url = json_string(value, "outbox").unwrap_or_default();
    let followers_url = json_string(value, "followers").unwrap_or_default();
    let following_url = json_string(value, "following").unwrap_or_default();
    let public_key_pem = nested_json_string(value, &["publicKey", "publicKeyPem"])
        .or_else(|| nested_json_string(value, &["public_key", "public_key_pem"]))
        .unwrap_or_default();
    let icon_url = extract_remote_icon_url(value);

    if preferred_username.trim().is_empty() || inbox_url.trim().is_empty() {
        return Err(federation_error(
            "remote actor is missing required user discovery fields",
        ));
    }

    sqlx::query(
        r#"
        INSERT INTO ap_remote_actor (
            actor_id,
            host,
            preferred_username,
            display_name,
            summary,
            inbox_url,
            shared_inbox_url,
            outbox_url,
            followers_url,
            following_url,
            public_key_pem,
            icon_url,
            status,
            discovered_at,
            last_refreshed_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'discovered', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (actor_id) DO UPDATE
        SET host = EXCLUDED.host,
            preferred_username = EXCLUDED.preferred_username,
            display_name = EXCLUDED.display_name,
            summary = EXCLUDED.summary,
            inbox_url = EXCLUDED.inbox_url,
            shared_inbox_url = EXCLUDED.shared_inbox_url,
            outbox_url = EXCLUDED.outbox_url,
            followers_url = EXCLUDED.followers_url,
            following_url = EXCLUDED.following_url,
            public_key_pem = EXCLUDED.public_key_pem,
            icon_url = EXCLUDED.icon_url,
            status = CASE
                WHEN COALESCE(NULLIF(ap_remote_actor.status, ''), '') = ''
                    THEN 'discovered'
                ELSE ap_remote_actor.status
            END,
            last_refreshed_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(&actor_id)
    .bind(&actor_host)
    .bind(&preferred_username)
    .bind(&display_name)
    .bind(&summary)
    .bind(&inbox_url)
    .bind(&shared_inbox_url)
    .bind(&outbox_url)
    .bind(&followers_url)
    .bind(&following_url)
    .bind(&public_key_pem)
    .bind(&icon_url)
    .execute(pool)
    .await
    .map_err(|err| federation_error(format!("failed to upsert remote actor: {err}")))?;

    ensure_discovered_instance(pool, &actor_host, &actor_id)
        .await
        .map_err(|err| federation_error(format!("failed to store discovered instance: {err}")))?;

    Ok(RemoteActorRow {
        actor_id,
        host: actor_host,
        preferred_username,
        display_name,
        summary,
        inbox_url,
        shared_inbox_url,
        outbox_url,
        followers_url,
        following_url,
        public_key_pem,
        icon_url,
        status: current_user_status,
        discovered_at: Some(Utc::now()),
        last_refreshed_at: Some(Utc::now()),
    })
}

fn map_remote_search_user(row: RemoteActorRow, username: &str, domain: &str) -> RemoteSearchUser {
    let display_name = row.display_name.trim();
    RemoteSearchUser {
        preferred_username: if display_name.is_empty() {
            row.preferred_username.clone()
        } else {
            display_name.to_string()
        },
        profile_photo_url: if row.icon_url.trim().is_empty() {
            "/public/avatar.webp".to_string()
        } else {
            row.icon_url.clone()
        },
        profile_photo_style: String::new(),
        profile_url: format!("/user/{}@{}", username.trim(), domain.trim()),
        handle: format!("@{}@{}", username, domain),
        is_remote: true,
    }
}

fn map_remote_search_view(row: RemoteActorRow) -> Option<crate::handler::SearchUserView> {
    let username = row.preferred_username.trim().to_string();
    let domain = row.host.trim().to_string();
    if username.is_empty() || domain.is_empty() {
        return None;
    }

    let remote = map_remote_search_user(row, &username, &domain);
    Some(crate::handler::SearchUserView {
        preferred_username: remote.preferred_username,
        profile_photo_url: remote.profile_photo_url,
        profile_photo_style: remote.profile_photo_style,
        profile_url: remote.profile_url,
        handle: remote.handle,
        is_remote: remote.is_remote,
    })
}

fn collection_url_or_default(base: &Url, suffix: &str) -> Url {
    base.join(suffix).unwrap_or_else(|_| base.clone())
}

impl FederatedPersonActor {
    fn to_person(&self) -> ActivityPubPerson {
        let outbox = self
            .outbox
            .clone()
            .unwrap_or_else(|| collection_url_or_default(&self.actor_id, "outbox"));
        let followers = self
            .followers
            .clone()
            .unwrap_or_else(|| collection_url_or_default(&self.actor_id, "followers"));
        let following = self
            .following
            .clone()
            .unwrap_or_else(|| collection_url_or_default(&self.actor_id, "following"));
        let icon = self.icon_url.as_ref().map(|url| ActivityPubImage {
            kind: Cow::Borrowed("Image"),
            media_type: Cow::Borrowed(guess_media_type(url)),
            url: url.clone(),
        });
        let image = self.image_url.as_ref().map(|url| ActivityPubImage {
            kind: Cow::Borrowed("Image"),
            media_type: Cow::Borrowed(guess_media_type(url)),
            url: url.clone(),
        });
        let public_profile_url = if self.local_user_id.is_some() {
            local_profile_url_from_actor(&self.actor_id, &self.preferred_username)
        } else {
            self.actor_id.clone()
        };

        ActivityPubPerson {
            id: self.actor_id.clone(),
            kind: Default::default(),
            preferred_username: self.preferred_username.clone(),
            name: self.display_name.clone(),
            summary: self.summary.clone(),
            inbox: self.inbox.clone(),
            outbox: outbox.clone(),
            followers,
            following,
            url: public_profile_url,
            discoverable: true,
            manually_approves_followers: false,
            published: self.created_at,
            public_key: PublicKey {
                id: format!("{}#main-key", self.actor_id),
                owner: self.actor_id.clone(),
                public_key_pem: self.public_key_pem.clone(),
            },
            icon,
            image,
            endpoints: self
                .shared_inbox
                .clone()
                .map(|shared_inbox| ActivityPubEndpoints {
                    shared_inbox: Some(shared_inbox),
                }),
        }
    }
}

#[async_trait]
impl Object for FederatedPersonActor {
    type DataType = PgPool;
    type Kind = ActivityPubPerson;
    type Error = ApError;

    fn last_refreshed_at(&self) -> Option<DateTime<Utc>> {
        self.last_refreshed_at
    }

    async fn read_from_id(
        object_id: Url,
        data: &Data<Self::DataType>,
    ) -> Result<Option<Self>, Self::Error> {
        if let Some(local_row) = load_local_actor_by_ap_id(data.app_data(), &object_id).await? {
            let base_url = format!(
                "{}://{}",
                object_id.scheme(),
                url_host_with_port(&object_id)
            );
            return Ok(Some(
                local_actor_from_row(data.app_data(), local_row, &base_url).await?,
            ));
        }

        load_remote_actor(data.app_data(), &object_id).await
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        Ok(self.to_person())
    }

    async fn verify(
        json: &Self::Kind,
        expected_domain: &Url,
        _data: &Data<Self::DataType>,
    ) -> Result<(), Self::Error> {
        verify_domains_match(&json.id, expected_domain)?;
        verify_domains_match(&json.inbox, &json.id)?;
        Ok(())
    }

    async fn from_json(json: Self::Kind, data: &Data<Self::DataType>) -> Result<Self, Self::Error> {
        let shared_inbox = json
            .endpoints
            .as_ref()
            .and_then(|endpoints| endpoints.shared_inbox.clone());

        sqlx::query(
            r#"
            INSERT INTO ap_remote_actor (
                actor_id,
                host,
                preferred_username,
                display_name,
                summary,
                inbox_url,
                shared_inbox_url,
                outbox_url,
                followers_url,
                following_url,
                public_key_pem,
                icon_url,
                status,
                discovered_at,
                last_refreshed_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'discovered', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT (actor_id) DO UPDATE
            SET host = EXCLUDED.host,
                preferred_username = EXCLUDED.preferred_username,
                display_name = EXCLUDED.display_name,
                summary = EXCLUDED.summary,
                inbox_url = EXCLUDED.inbox_url,
                shared_inbox_url = EXCLUDED.shared_inbox_url,
                outbox_url = EXCLUDED.outbox_url,
                followers_url = EXCLUDED.followers_url,
                following_url = EXCLUDED.following_url,
                public_key_pem = EXCLUDED.public_key_pem,
                icon_url = EXCLUDED.icon_url,
                status = CASE
                    WHEN COALESCE(NULLIF(ap_remote_actor.status, ''), '') = ''
                        THEN 'discovered'
                    ELSE ap_remote_actor.status
                END,
                last_refreshed_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(json.id.as_str())
        .bind(json.id.host_str().unwrap_or_default().to_ascii_lowercase())
        .bind(json.preferred_username.trim())
        .bind(json.name.trim())
        .bind(json.summary.trim())
        .bind(json.inbox.as_str())
        .bind(shared_inbox.as_ref().map(Url::as_str).unwrap_or(""))
        .bind(json.outbox.as_str())
        .bind(json.followers.as_str())
        .bind(json.following.as_str())
        .bind(json.public_key.public_key_pem.as_str())
        .bind(json.icon.as_ref().map(|icon| icon.url.as_str()).unwrap_or(""))
        .execute(data.app_data())
        .await
        .map_err(|err| federation_error(format!("failed to persist remote actor: {err}")))?;

        remote_actor_from_row(RemoteActorRow {
            actor_id: json.id.to_string(),
            host: json.id.host_str().unwrap_or_default().to_ascii_lowercase(),
            preferred_username: json.preferred_username,
            display_name: json.name,
            summary: json.summary,
            inbox_url: json.inbox.to_string(),
            shared_inbox_url: shared_inbox.map(|url| url.to_string()).unwrap_or_default(),
            outbox_url: json.outbox.to_string(),
            followers_url: json.followers.to_string(),
            following_url: json.following.to_string(),
            public_key_pem: json.public_key.public_key_pem,
            icon_url: json
                .icon
                .map(|icon| icon.url.to_string())
                .unwrap_or_default(),
            status: "discovered".to_string(),
            discovered_at: Some(Utc::now()),
            last_refreshed_at: Some(Utc::now()),
        })
    }
}

impl Actor for FederatedPersonActor {
    fn id(&self) -> Url {
        self.actor_id.clone()
    }

    fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        self.private_key_pem.clone()
    }

    fn inbox(&self) -> Url {
        self.inbox.clone()
    }

    fn shared_inbox(&self) -> Option<Url> {
        self.shared_inbox.clone()
    }
}

async fn persist_remote_create_post(
    pool: &PgPool,
    activity: &CreatePostActivity,
) -> Result<(), ApError> {
    persist_remote_post_from_note(
        pool,
        activity.id.as_str(),
        &activity.actor,
        &activity.object,
        is_public_create_post(activity),
    )
    .await
}

async fn persist_remote_post_from_note(
    pool: &PgPool,
    activity_id: &str,
    actor: &Url,
    note: &ActivityPubNote,
    is_public: bool,
) -> Result<(), ApError> {
    if actor != &note.attributed_to {
        return Err(federation_error(
            "post activity actor does not match note author",
        ));
    }
    if !is_public {
        return Ok(());
    }

    let actor_host = actor
        .host_str()
        .map(|host| host.to_ascii_lowercase())
        .unwrap_or_default();
    if actor_host.is_empty() || actor_host == CANONICAL_INSTAVOX_DOMAIN {
        return Ok(());
    }

    let instance_status = get_instance_status(pool, &actor_host)
        .await
        .map_err(|err| federation_error(format!("failed to load instance status: {err}")))?;
    if instance_status == "limited" || instance_status == "ban" {
        return Ok(());
    }

    let Some(remote_actor) = load_remote_actor(pool, actor).await? else {
        return Ok(());
    };
    if remote_actor.local_user_id.is_some() {
        return Ok(());
    }

    let remote_user_status = get_remote_user_status(pool, actor.as_str())
        .await
        .map_err(|err| federation_error(format!("failed to load remote user status: {err}")))?;
    if remote_user_status == "limited" || remote_user_status == "ban" {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO ap_remote_post (
            object_id,
            activity_id,
            actor_id,
            host,
            content,
            url,
            published_at,
            discovered_at,
            updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (object_id) DO UPDATE
        SET activity_id = EXCLUDED.activity_id,
            actor_id = EXCLUDED.actor_id,
            host = EXCLUDED.host,
            content = EXCLUDED.content,
            url = EXCLUDED.url,
            published_at = EXCLUDED.published_at,
            updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(note.id.as_str())
    .bind(activity_id)
    .bind(actor.as_str())
    .bind(actor_host)
    .bind(note.content.trim())
    .bind(note.url.as_str())
    .bind(note.published)
    .execute(pool)
    .await
    .map_err(|err| federation_error(format!("failed to persist remote post: {err}")))?;

    Ok(())
}

async fn persist_remote_person_update(
    pool: &PgPool,
    activity: &UpdatePersonActivity,
) -> Result<(), ApError> {
    if activity.actor != activity.object.id {
        return Err(federation_error(
            "update actor does not match person object id",
        ));
    }

    let actor_host = activity
        .actor
        .host_str()
        .map(|host| host.to_ascii_lowercase())
        .unwrap_or_default();
    if actor_host.is_empty() || actor_host == CANONICAL_INSTAVOX_DOMAIN {
        return Ok(());
    }

    let instance_status = get_instance_status(pool, &actor_host)
        .await
        .map_err(|err| federation_error(format!("failed to load instance status: {err}")))?;
    if instance_status == "limited" || instance_status == "ban" {
        return Ok(());
    }

    let remote_user_status = get_remote_user_status(pool, activity.actor.as_str())
        .await
        .map_err(|err| federation_error(format!("failed to load remote user status: {err}")))?;
    if remote_user_status == "limited" || remote_user_status == "ban" {
        return Ok(());
    }

    let actor_value = serde_json::to_value(&activity.object).map_err(|err| {
        federation_error(format!("failed to serialize remote actor update: {err}"))
    })?;
    let _ = upsert_remote_actor_from_value(pool, activity.object.id.as_str(), &actor_value).await?;
    Ok(())
}

#[async_trait]
impl ActivityHandler for CreatePostActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        &self.actor
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        persist_remote_create_post(data.app_data(), &self).await
    }
}

#[async_trait]
impl ActivityHandler for UpdatePersonActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        &self.actor
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        if self.actor != self.object.id {
            return Err(federation_error(
                "update actor does not match person object id",
            ));
        }
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        persist_remote_person_update(data.app_data(), &self).await
    }
}

#[async_trait]
impl ActivityHandler for UpdatePostActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        &self.actor
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        if self.actor != self.object.attributed_to {
            return Err(federation_error("update actor does not match note author"));
        }
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        persist_remote_post_from_note(
            data.app_data(),
            self.id.as_str(),
            &self.actor,
            &self.object,
            is_public_post_update(&self),
        )
        .await
    }
}

#[async_trait]
impl ActivityHandler for FollowActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let remote_actor = self.actor.dereference(data).await?;
        let local_actor = self.object.dereference(data).await?;

        let Some(local_user_id) = local_actor.local_user_id else {
            return Err(federation_error("follow target must be a local user"));
        };

        sqlx::query(
            r#"
            INSERT INTO ap_remote_follow (
                local_user_id,
                remote_actor_id,
                activity_id,
                status,
                created_at,
                updated_at
            )
            VALUES ($1, $2, $3, 'accepted', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT (local_user_id, remote_actor_id) DO UPDATE
            SET activity_id = EXCLUDED.activity_id,
                status = 'accepted',
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(local_user_id)
        .bind(remote_actor.actor_id.as_str())
        .bind(self.id.as_str())
        .execute(data.app_data())
        .await
        .map_err(|err| federation_error(format!("failed to persist remote follow: {err}")))?;

        send_follow_accept(data, &local_actor, &remote_actor, self).await
    }
}

#[async_trait]
impl ActivityHandler for UndoActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let remote_actor = self.actor.dereference(data).await?;

        match self.object {
            UndoObject::Follow(follow) => {
                let local_actor = follow.object.dereference(data).await?;
                if let Some(local_user_id) = local_actor.local_user_id {
                    sqlx::query(
                        r#"
                        DELETE FROM ap_remote_follow
                        WHERE local_user_id = $1
                          AND remote_actor_id = $2
                        "#,
                    )
                    .bind(local_user_id)
                    .bind(remote_actor.actor_id.as_str())
                    .execute(data.app_data())
                    .await
                    .map_err(|err| {
                        federation_error(format!("failed to delete remote follow via undo: {err}"))
                    })?;
                }
            }
            UndoObject::Id(activity_id) => {
                sqlx::query(
                    r#"
                    DELETE FROM ap_remote_follow
                    WHERE remote_actor_id = $1
                      AND activity_id = $2
                    "#,
                )
                .bind(remote_actor.actor_id.as_str())
                .bind(activity_id.as_str())
                .execute(data.app_data())
                .await
                .map_err(|err| {
                    federation_error(format!(
                        "failed to delete remote follow by activity id: {err}"
                    ))
                })?;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for AcceptActivity {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for PersonAcceptedActivities {
    type DataType = PgPool;
    type Error = ApError;

    fn id(&self) -> &Url {
        match self {
            Self::Create(activity) => activity.id(),
            Self::UpdatePerson(activity) => activity.id(),
            Self::UpdatePost(activity) => activity.id(),
            Self::Follow(activity) => activity.id(),
            Self::Undo(activity) => activity.id(),
        }
    }

    fn actor(&self) -> &Url {
        match self {
            Self::Create(activity) => activity.actor(),
            Self::UpdatePerson(activity) => activity.actor(),
            Self::UpdatePost(activity) => activity.actor(),
            Self::Follow(activity) => activity.actor(),
            Self::Undo(activity) => activity.actor(),
        }
    }

    async fn verify(&self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            Self::Create(activity) => activity.verify(data).await,
            Self::UpdatePerson(activity) => activity.verify(data).await,
            Self::UpdatePost(activity) => activity.verify(data).await,
            Self::Follow(activity) => activity.verify(data).await,
            Self::Undo(activity) => activity.verify(data).await,
        }
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            Self::Create(activity) => activity.receive(data).await,
            Self::UpdatePerson(activity) => activity.receive(data).await,
            Self::UpdatePost(activity) => activity.receive(data).await,
            Self::Follow(activity) => activity.receive(data).await,
            Self::Undo(activity) => activity.receive(data).await,
        }
    }
}

async fn ordered_collection_response(id: Url, items: Vec<Url>) -> Response {
    let body = OrderedCollection {
        id,
        kind: Cow::Borrowed("OrderedCollection"),
        total_items: items.len(),
        ordered_items: items,
    };

    (
        [(CONTENT_TYPE, FEDERATION_CONTENT_TYPE)],
        Json(WithContext::new_default(body)),
    )
        .into_response()
}
