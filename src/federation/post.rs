/*  -----------------------------------------------------
    |                                                   |
    | Local Post section                                |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Debug, Clone, sqlx::FromRow)]
struct LocalPostRow {
    post_id: i64,
    user_id: i32,
    body: String,
    link_url: String,
    visibility: String,
    community_id: Option<i64>,
    created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct LocalPostImageRow {
    image_url: String,
    mime_type: String,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePostActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: CreateType,
    actor: Url,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to: Vec<Url>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cc: Vec<Url>,
    object: ActivityPubNote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePostActivity {
    id: Url,
    #[serde(rename = "type")]
    kind: UpdateType,
    actor: Url,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to: Vec<Url>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cc: Vec<Url>,
    object: ActivityPubNote,
}


fn is_public_create_post(activity: &CreatePostActivity) -> bool {
    activity_or_object_is_public(
        &activity.to,
        &activity.cc,
        &activity.object.to,
        &activity.object.cc,
    )
}

fn is_public_post_update(activity: &UpdatePostActivity) -> bool {
    activity_or_object_is_public(
        &activity.to,
        &activity.cc,
        &activity.object.to,
        &activity.object.cc,
    )
}

fn post_create_activity_url(base_url: &str, post_id: i64) -> Result<Url, ApError> {
    Url::parse(&format!("{base_url}/ap/posts/{post_id}/activities/create"))
        .map_err(|err| federation_error(format!("invalid post create activity url: {err}")))
}



/*  -----------------------------------------------------
    |                                                   |
    | Remote Post section                               |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(Debug, Clone, sqlx::FromRow)]
struct RemotePostSearchRow {
    object_id: String,
    post_url: String,
    content: String,
    created_at: String,
    actor_id: String,
    preferred_username: String,
    display_name: String,
    icon_url: String,
}



pub async fn send_post_to_subscribed_remote_inboxes(
    pool: &PgPool,
    author_user_id: i32,
    post_id: i64,
) -> Result<(), ApError> {
    let Some(post) = load_local_post(pool, post_id).await? else {
        return Ok(());
    };
    if post.user_id != author_user_id || post.community_id.is_some() {
        return Ok(());
    }

    let visibility = post.visibility.trim().to_ascii_lowercase();
    if visibility != "public" {
        return Ok(());
    }

    let inboxes = remote_subscribed_post_inboxes(pool).await?;
    if inboxes.is_empty() {
        return Ok(());
    }

    let base_url = format!("https://{CANONICAL_INSTAVOX_DOMAIN}");
    let Some(row) = load_local_actor_by_user_id(pool, author_user_id).await? else {
        return Ok(());
    };
    let local_actor = local_actor_from_row(pool, row, &base_url).await?;
    let note = local_post_to_note(pool, &post, &local_actor, &base_url).await?;
    let activity = CreatePostActivity {
        id: post_create_activity_url(&base_url, post.post_id)?,
        kind: Default::default(),
        actor: local_actor.actor_id.clone(),
        to: note.to.clone(),
        cc: note.cc.clone(),
        object: note,
    };
    let activity = WithContext::new_default(activity);
    let data = federation_request_data(pool, CANONICAL_INSTAVOX_DOMAIN, false).await?;
    let sends = SendActivityTask::prepare(&activity, &local_actor, inboxes, &data).await?;
    for send in sends {
        if let Err(err) = send.sign_and_send(&data).await {
            tracing::warn!(
                "failed to send post {} to subscribed ActivityPub inbox: {}",
                post_id,
                err
            );
        }
    }

    Ok(())
}

pub async fn activitypub_post(
    Path(post_id): Path<i64>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let post = match load_local_post(&pool, post_id).await {
        Ok(Some(post)) => post,
        Ok(None) => return (StatusCode::NOT_FOUND, "Post not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_post query failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load post").into_response();
        }
    };

    let visibility = post.visibility.trim().to_ascii_lowercase();
    if post.community_id.is_some() || visibility == "private" || visibility == "friends" {
        return (StatusCode::NOT_FOUND, "Post not found").into_response();
    }

    let actor_row = match load_local_actor_by_user_id(&pool, post.user_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "Post not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_post actor query failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load post actor",
            )
                .into_response();
        }
    };
    let actor = match local_actor_from_row(&pool, actor_row, &base_url).await {
        Ok(actor) => actor,
        Err(err) => {
            tracing::warn!("activitypub_post actor build failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load post actor",
            )
                .into_response();
        }
    };
    let note = match local_post_to_note(&pool, &post, &actor, &base_url).await {
        Ok(note) => note,
        Err(err) => {
            tracing::warn!("activitypub_post note build failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load post").into_response();
        }
    };

    (
        [(CONTENT_TYPE, FEDERATION_CONTENT_TYPE)],
        Json(WithContext::new_default(note)),
    )
        .into_response()
}
