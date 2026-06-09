/*  -----------------------------------------------------
    |                                                   |
    | Federation Data section                           |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Debug, Clone)]
struct UserActivityPubData {
    public_key_pem: String,
    private_key_pem: Option<String>,
    actor_id: String,
    inbox: String,
    outbox: String,
    followers: String,
    following: String,
}


#[derive(Debug, Clone)]
struct FederatedPersonActor {
    local_user_id: Option<i32>,
    preferred_username: String,
    display_name: String,
    summary: String,
    created_at: Option<DateTime<Utc>>,
    actor_id: Url,
    inbox: Url,
    outbox: Option<Url>,
    followers: Option<Url>,
    following: Option<Url>,
    public_key_pem: String,
    private_key_pem: Option<String>,
    shared_inbox: Option<Url>,
    icon_url: Option<Url>,
    image_url: Option<Url>,
    last_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPubPerson {
    id: Url,
    #[serde(rename = "type")]
    kind: PersonType,
    preferred_username: String,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    summary: String,
    inbox: Url,
    outbox: Url,
    followers: Url,
    following: Url,
    url: Url,
    discoverable: bool,
    manually_approves_followers: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<DateTime<Utc>>,
    public_key: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<ActivityPubImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<ActivityPubImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoints: Option<ActivityPubEndpoints>,
}

fn actor_display_name(
    preferred_username: &str,
    display_name_mode: &str,
    first_name: &str,
    first_name_public: bool,
    last_name: &str,
    last_name_public: bool,
) -> String {
    if display_name_mode
        .trim()
        .eq_ignore_ascii_case("preferred_username")
    {
        return preferred_username.to_string();
    }

    let first_name = if first_name_public {
        first_name.trim()
    } else {
        ""
    };
    let last_name = if last_name_public {
        last_name.trim()
    } else {
        ""
    };
    let full = format!("{} {}", first_name, last_name).trim().to_string();
    if full.is_empty() {
        preferred_username.to_string()
    } else {
        full
    }
}

fn profile_update_activity_url(actor_id: &Url) -> Result<Url, ApError> {
    Url::parse(&format!(
        "{}/activities/update/{}",
        actor_id.as_str().trim_end_matches('/'),
        Utc::now().timestamp_micros()
    ))
    .map_err(|err| federation_error(format!("invalid profile update activity url: {err}")))
}



/*  -----------------------------------------------------
    |                                                   |
    | Local User section                                |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(sqlx::FromRow)]
struct ActivityPubUserRow {
    id: i32,
    preferred_username: String,
    ap_id: Option<String>,
    ap_inbox: Option<String>,
    ap_outbox: Option<String>,
    ap_public_key: Option<String>,
    ap_private_key: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct LocalActorRow {
    user_id: i32,
    preferred_username: String,
    federation_display_name_mode: String,
    first_name: String,
    first_name_public: bool,
    last_name: String,
    last_name_public: bool,
    bio_description: Option<String>,
    profile_photo_url: Option<String>,
    background_photo_url: Option<String>,
    created_at: Option<DateTime<Utc>>,
    ap_id: Option<String>,
    ap_inbox: Option<String>,
    ap_outbox: Option<String>,
    ap_public_key: Option<String>,
    ap_private_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePersonActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: UpdateType,
    actor: Url,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to: Vec<Url>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cc: Vec<Url>,
    object: ActivityPubPerson,
}


fn local_profile_path(username: &str) -> String {
    format!("/user/{}@{}", username.trim(), CANONICAL_INSTAVOX_DOMAIN)
}

fn local_profile_url_from_actor(actor_id: &Url, username: &str) -> Url {
    let base = format!("{}://{}", actor_id.scheme(), url_host_with_port(actor_id));
    Url::parse(&format!("{base}{}", local_profile_path(username)))
        .unwrap_or_else(|_| actor_id.clone())
}

pub async fn send_profile_update_to_mastodon_followers(
    pool: &PgPool,
    user_id: i32,
) -> Result<(), ApError> {
    let inboxes = remote_follower_inboxes(pool, user_id).await?;
    if inboxes.is_empty() {
        return Ok(());
    }

    let base_url = format!("https://{CANONICAL_INSTAVOX_DOMAIN}");
    let Some(row) = load_local_actor_by_user_id(pool, user_id).await? else {
        return Ok(());
    };
    let local_actor = local_actor_from_row(pool, row, &base_url).await?;
    let followers = followers_url_from_actor(&local_actor.actor_id)?;
    let activity = UpdatePersonActivity {
        id: profile_update_activity_url(&local_actor.actor_id)?,
        kind: Default::default(),
        actor: local_actor.actor_id.clone(),
        to: vec![public()],
        cc: vec![followers],
        object: local_actor.to_person(),
    };
    let activity = WithContext::new_default(activity);
    let data = federation_request_data(pool, CANONICAL_INSTAVOX_DOMAIN, false).await?;
    let sends = SendActivityTask::prepare(&activity, &local_actor, inboxes, &data).await?;
    for send in sends {
        if let Err(err) = send.sign_and_send(&data).await {
            tracing::warn!(
                "failed to send profile update for user {} to ActivityPub follower inbox: {}",
                user_id,
                err
            );
        }
    }

    Ok(())
}


async fn load_local_actor_for_request(
    pool: &PgPool,
    requested_username: &str,
    base_url: &str,
) -> Result<Option<FederatedPersonActor>, ApError> {
    let Some(row) = load_local_actor_by_identifier(pool, requested_username).await? else {
        return Ok(None);
    };

    local_actor_from_row(pool, row, base_url).await.map(Some)
}



/*  -----------------------------------------------------
    |                                                   |
    | Remote User section                               |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Debug, Clone, sqlx::FromRow)]
struct RemoteActorRow {
    actor_id: String,
    host: String,
    preferred_username: String,
    display_name: String,
    summary: String,
    inbox_url: String,
    shared_inbox_url: String,
    outbox_url: String,
    followers_url: String,
    following_url: String,
    public_key_pem: String,
    icon_url: String,
    status: String,
    discovered_at: Option<DateTime<Utc>>,
    last_refreshed_at: Option<DateTime<Utc>>,
}


pub struct RemoteSearchUser {
    pub preferred_username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub profile_url: String,
    pub handle: String,
    pub is_remote: bool,
}


fn build_actor_url_strings(base_url: &str, preferred_username: &str) -> UserActivityPubData {
    let encoded = urlencoding::encode(preferred_username);
    let actor_id = format!("{base_url}/ap/users/{encoded}");
    UserActivityPubData {
        actor_id: actor_id.clone(),
        inbox: format!("{actor_id}/inbox"),
        outbox: format!("{actor_id}/outbox"),
        followers: format!("{actor_id}/followers"),
        following: format!("{actor_id}/following"),
        public_key_pem: String::new(),
        private_key_pem: None,
    }
}



/*  -----------------------------------------------------
    |                                                   |
    | Follower section                                  |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Debug, Clone, sqlx::FromRow)]
struct RemoteFollowerInboxRow {
    inbox_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: FollowType,
    actor: ObjectId<FederatedPersonActor>,
    object: ObjectId<FederatedPersonActor>,
}


async fn send_follow_accept(
    data: &Data<PgPool>,
    local_actor: &FederatedPersonActor,
    remote_actor: &FederatedPersonActor,
    follow: FollowActivity,
) -> Result<(), ApError> {
    let accept_id = Url::parse(&format!(
        "{}/accept/{}",
        local_actor.actor_id,
        Utc::now().timestamp_micros()
    ))
    .map_err(|err| federation_error(format!("failed to build accept activity id: {err}")))?;

    let accept = AcceptActivity {
        id: accept_id,
        kind: Default::default(),
        actor: ObjectId::from(local_actor.actor_id.clone()),
        object: follow,
    };

    let sends = SendActivityTask::prepare(
        &accept,
        local_actor,
        vec![remote_actor.shared_inbox_or_inbox()],
        data,
    )
    .await?;
    for send in sends {
        send.sign_and_send(data).await?;
    }
    Ok(())
}


async fn local_followers_urls(pool: &PgPool, local_user_id: i32) -> Result<Vec<Url>, ApError> {
    let local_followers = sqlx::query_scalar::<_, String>(
        r#"
        SELECT u.ap_id
        FROM relationship r
        INNER JOIN users u ON u.id = r.sender_id
        WHERE r.receiver_id = $1
          AND LOWER(COALESCE(r.status, '')) IN ('follow', 'following', 'follower')
          AND COALESCE(u.ap_id, '') <> ''
        ORDER BY r.friendship_id DESC
        "#,
    )
    .bind(local_user_id)
    .fetch_all(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load local followers: {err}")))?;

    let remote_followers = sqlx::query_scalar::<_, String>(
        r#"
        SELECT remote_actor_id
        FROM ap_remote_follow
        WHERE local_user_id = $1
          AND status = 'accepted'
        ORDER BY created_at DESC
        "#,
    )
    .bind(local_user_id)
    .fetch_all(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load remote followers: {err}")))?;

    Ok(local_followers
        .into_iter()
        .chain(remote_followers.into_iter())
        .filter_map(|value| Url::parse(&value).ok())
        .collect())
}

async fn local_following_urls(pool: &PgPool, local_user_id: i32) -> Result<Vec<Url>, ApError> {
    let rows = sqlx::query_scalar::<_, String>(
        r#"
        SELECT u.ap_id
        FROM relationship r
        INNER JOIN users u ON u.id = r.receiver_id
        WHERE r.sender_id = $1
          AND LOWER(COALESCE(r.status, '')) IN ('follow', 'following', 'follower')
          AND COALESCE(u.ap_id, '') <> ''
        ORDER BY r.friendship_id DESC
        "#,
    )
    .bind(local_user_id)
    .fetch_all(pool)
    .await
    .map_err(|err| federation_error(format!("failed to load following list: {err}")))?;

    Ok(rows
        .into_iter()
        .filter_map(|value| Url::parse(&value).ok())
        .collect())
}


fn followers_url_from_actor(actor_id: &Url) -> Result<Url, ApError> {
    actor_id
        .join("followers")
        .map_err(|err| federation_error(format!("invalid actor followers url: {err}")))
}

pub async fn activitypub_user(
    Path(requested_username): Path<String>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let actor = match load_local_actor_for_request(&pool, &requested_username, &base_url).await {
        Ok(Some(actor)) => actor,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load actor").into_response();
        }
    };

    if prefers_browser_html(&headers) {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(LOCATION, local_profile_path(&actor.preferred_username))],
        )
            .into_response();
    }

    (
        [(CONTENT_TYPE, FEDERATION_CONTENT_TYPE)],
        Json(WithContext::new_default(actor.to_person())),
    )
        .into_response()
}


pub async fn activitypub_user_followers(
    Path(requested_username): Path<String>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let actor = match load_local_actor_for_request(&pool, &requested_username, &base_url).await {
        Ok(Some(actor)) => actor,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user_followers failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load actor").into_response();
        }
    };

    let Some(local_user_id) = actor.local_user_id else {
        return (StatusCode::NOT_FOUND, "User not found").into_response();
    };

    let items = match local_followers_urls(&pool, local_user_id).await {
        Ok(items) => items,
        Err(err) => {
            tracing::warn!("activitypub_user_followers query failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load followers",
            )
                .into_response();
        }
    };

    let collection_id = actor
        .followers
        .clone()
        .unwrap_or_else(|| collection_url_or_default(&actor.actor_id, "followers"));
    ordered_collection_response(collection_id, items).await
}

pub async fn activitypub_user_following(
    Path(requested_username): Path<String>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let actor = match load_local_actor_for_request(&pool, &requested_username, &base_url).await {
        Ok(Some(actor)) => actor,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user_following failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load actor").into_response();
        }
    };

    let Some(local_user_id) = actor.local_user_id else {
        return (StatusCode::NOT_FOUND, "User not found").into_response();
    };

    let items = match local_following_urls(&pool, local_user_id).await {
        Ok(items) => items,
        Err(err) => {
            tracing::warn!("activitypub_user_following query failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load following",
            )
                .into_response();
        }
    };

    let collection_id = actor
        .following
        .clone()
        .unwrap_or_else(|| collection_url_or_default(&actor.actor_id, "following"));
    ordered_collection_response(collection_id, items).await
}


pub async fn activitypub_user_inbox(
    Path(requested_username): Path<String>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
    body: Body,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };
    let scheme = request_scheme(&headers);
    let base_url = format!("{scheme}://{host}");
    let allow_http = scheme == "http"
        || host.starts_with("127.0.0.1")
        || host.starts_with("localhost")
        || host.starts_with("[::1]");

    match load_local_actor_for_request(&pool, &requested_username, &base_url).await {
        Ok(Some(_)) => {}
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user_inbox user bootstrap failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load actor").into_response();
        }
    }

    let data = match federation_request_data(&pool, &host, allow_http).await {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!("activitypub_user_inbox config failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to prepare federation config",
            )
                .into_response();
        }
    };

    let body = match to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!("activitypub_user_inbox body read failed: {}", err);
            return (StatusCode::BAD_REQUEST, "Invalid request body").into_response();
        }
    };

    let activity = match serde_json::from_slice::<PersonAcceptedActivities>(&body) {
        Ok(activity) => activity,
        Err(err) => {
            tracing::warn!("activitypub_user_inbox parse failed: {}", err);
            return (StatusCode::BAD_REQUEST, "Invalid ActivityPub payload").into_response();
        }
    };

    if let Err(err) = activity.verify(&data).await {
        tracing::warn!("activitypub_user_inbox verify failed: {}", err);
        return (StatusCode::BAD_REQUEST, "Invalid ActivityPub activity").into_response();
    }

    match activity.receive(&data).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user_inbox receive failed: {}", err);
            (
                StatusCode::BAD_REQUEST,
                "Failed to process ActivityPub activity",
            )
                .into_response()
        }
    }
}

pub async fn activitypub_user_outbox(
    Path(requested_username): Path<String>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let actor = match load_local_actor_for_request(&pool, &requested_username, &base_url).await {
        Ok(Some(actor)) => actor,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_user_outbox failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load actor").into_response();
        }
    };

    let outbox_id = actor
        .outbox
        .clone()
        .unwrap_or_else(|| collection_url_or_default(&actor.actor_id, "outbox"));
    ordered_collection_response(outbox_id, Vec::new()).await
}
