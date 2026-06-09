
fn detect_instance_protocol(sample: &str, host: &str) -> String {
    let haystack = format!(
        "{} {}",
        host.to_ascii_lowercase(),
        sample.to_ascii_lowercase()
    );
    if haystack.contains("threads.net") {
        return "Threads".to_string();
    }
    if haystack.contains("pixelfed") {
        return "PixelFed".to_string();
    }
    if haystack.contains("mastodon") {
        return "Mastodon".to_string();
    }
    if haystack.contains("pleroma") || haystack.contains("akkoma") {
        return "Pleroma/Akkoma".to_string();
    }
    if haystack.contains("misskey") || haystack.contains("calckey") || haystack.contains("firefish")
    {
        return "Misskey/Firefish".to_string();
    }
    if haystack.contains("lemmy") {
        return "Lemmy".to_string();
    }
    "Other ActivityPub".to_string()
}

async fn ensure_discovered_instance(
    pool: &PgPool,
    host: &str,
    sample: &str,
) -> Result<String, sqlx::Error> {
    let normalized_host = host.trim().to_ascii_lowercase();
    if normalized_host.is_empty() || normalized_host == CANONICAL_INSTAVOX_DOMAIN {
        return Ok("discovered".to_string());
    }

    let protocol = detect_instance_protocol(sample, &normalized_host);
    sqlx::query(
        r#"
        INSERT INTO discovered_instance (host, protocol, status, discovered_at, last_seen_at)
        VALUES ($1, $2, 'discovered', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (host) DO UPDATE
        SET protocol = CASE
                WHEN COALESCE(NULLIF(discovered_instance.protocol, ''), '') = ''
                    THEN EXCLUDED.protocol
                ELSE discovered_instance.protocol
            END,
            last_seen_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(&normalized_host)
    .bind(&protocol)
    .execute(pool)
    .await?;

    sqlx::query_scalar::<_, String>(
        r#"
        SELECT LOWER(COALESCE(NULLIF(status, ''), 'discovered'))
        FROM discovered_instance
        WHERE host = $1
        LIMIT 1
        "#,
    )
    .bind(&normalized_host)
    .fetch_optional(pool)
    .await
    .map(|value| normalize_discovery_status(&value.unwrap_or_else(|| "discovered".to_string())))
}

async fn get_instance_status(pool: &PgPool, host: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        r#"
        SELECT LOWER(COALESCE(NULLIF(status, ''), 'discovered'))
        FROM discovered_instance
        WHERE host = LOWER($1)
        LIMIT 1
        "#,
    )
    .bind(host)
    .fetch_optional(pool)
    .await
    .map(|value| normalize_discovery_status(&value.unwrap_or_else(|| "discovered".to_string())))
}

pub async fn activitypub_webfinger(
    Query(query): Query<WebfingerQuery>,
    headers: HeaderMap,
    State(pool): State<PgPool>,
) -> Response {
    let Some(host) = request_host(&headers) else {
        return (StatusCode::BAD_REQUEST, "Missing host header").into_response();
    };

    let (requested_username, requested_domain) = match parse_webfinger_resource(&query.resource) {
        Some(parts) => parts,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Invalid webfinger resource, expected acct:user@domain",
            )
                .into_response();
        }
    };

    if !host_matches_resource(&host, &requested_domain) {
        return (StatusCode::NOT_FOUND, "Resource is not hosted here").into_response();
    }

    let user = match sqlx::query_as::<_, ActivityPubUserRow>(
        r#"
        SELECT
            id,
            preferred_username,
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
    .bind(&requested_username)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(user)) => user,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(err) => {
            tracing::warn!("activitypub_webfinger query failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load user").into_response();
        }
    };

    let base_url = format!("{}://{}", request_scheme(&headers), host);
    let actor_data = match ensure_user_activitypub_data(
        &pool,
        user.id,
        &user.preferred_username,
        &base_url,
        user.ap_id.as_deref(),
        user.ap_inbox.as_deref(),
        user.ap_outbox.as_deref(),
        user.ap_public_key.as_deref(),
        user.ap_private_key.as_deref(),
    )
    .await
    {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!("activitypub_webfinger data bootstrap failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load actor data",
            )
                .into_response();
        }
    };

    let actor_url = match Url::parse(&actor_data.actor_id) {
        Ok(url) => url,
        Err(err) => {
            tracing::warn!("activitypub_webfinger actor url parse failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Invalid actor id").into_response();
        }
    };

    let canonical_subject = format!("acct:{}@{}", user.preferred_username, requested_domain);
    Json(build_webfinger_response(canonical_subject, actor_url)).into_response()
}

pub async fn ensure_federation_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS discovered_instance (
            host TEXT PRIMARY KEY,
            protocol TEXT NOT NULL DEFAULT 'Other ActivityPub',
            status TEXT NOT NULL DEFAULT 'discovered',
            discovered_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_seen_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS discovered_instance_status_last_seen_idx
        ON discovered_instance (status, last_seen_at DESC, host ASC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ap_remote_actor (
            actor_id TEXT PRIMARY KEY,
            host TEXT NOT NULL DEFAULT '',
            preferred_username TEXT NOT NULL DEFAULT '',
            display_name TEXT NOT NULL DEFAULT '',
            summary TEXT NOT NULL DEFAULT '',
            inbox_url TEXT NOT NULL DEFAULT '',
            shared_inbox_url TEXT NOT NULL DEFAULT '',
            outbox_url TEXT NOT NULL DEFAULT '',
            followers_url TEXT NOT NULL DEFAULT '',
            following_url TEXT NOT NULL DEFAULT '',
            public_key_pem TEXT NOT NULL DEFAULT '',
            icon_url TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'discovered',
            discovered_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_refreshed_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE discovered_instance
        ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'discovered'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE discovered_instance
        SET status = CASE
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('authorized', 'discover', 'discovered', 'allow', 'allowed')
                THEN 'discovered'
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('limited', 'limit')
                THEN 'limited'
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('blocked', 'block', 'ban', 'banned')
                THEN 'ban'
            ELSE 'discovered'
        END
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE ap_remote_actor
        ADD COLUMN IF NOT EXISTS host TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE ap_remote_actor
        ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'discovered'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE ap_remote_actor
        ADD COLUMN IF NOT EXISTS discovered_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE ap_remote_actor
        SET host = LOWER(REGEXP_REPLACE(actor_id, '^[a-z]+://([^/]+)/?.*$', '\1'))
        WHERE BTRIM(COALESCE(host, '')) = ''
          AND actor_id ~ '^[a-z]+://'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE ap_remote_actor
        SET status = CASE
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('authorized', 'discover', 'discovered', 'allow', 'allowed')
                THEN 'discovered'
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('limited', 'limit')
                THEN 'limited'
            WHEN LOWER(COALESCE(NULLIF(status, ''), 'discovered')) IN ('blocked', 'block', 'ban', 'banned')
                THEN 'ban'
            ELSE 'discovered'
        END
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ap_remote_follow (
            local_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            remote_actor_id TEXT NOT NULL,
            activity_id TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'accepted',
            created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (local_user_id, remote_actor_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ap_remote_post (
            object_id TEXT PRIMARY KEY,
            activity_id TEXT NOT NULL DEFAULT '',
            actor_id TEXT NOT NULL,
            host TEXT NOT NULL DEFAULT '',
            content TEXT NOT NULL DEFAULT '',
            url TEXT NOT NULL DEFAULT '',
            published_at TIMESTAMPTZ,
            discovered_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_ap_remote_post_actor_updated
        ON ap_remote_post(actor_id, updated_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_ap_remote_post_host_updated
        ON ap_remote_post(host, updated_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_ap_remote_follow_activity_id
        ON ap_remote_follow(activity_id)
        "#,
    )
    .execute(pool)
    .await?;

    let legacy_actor_ids = sqlx::query_scalar::<_, String>(
        r#"
        SELECT ap_id
        FROM users
        WHERE COALESCE(ap_local, TRUE) = FALSE
          AND BTRIM(COALESCE(ap_id, '')) <> ''
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for actor_id in legacy_actor_ids {
        if let Ok(parsed) = Url::parse(&actor_id) {
            if let Some(host) = parsed.host_str() {
                let _ = ensure_discovered_instance(pool, host, &actor_id).await;
            }
        }
    }

    let cached_actor_ids = sqlx::query_scalar::<_, String>(
        r#"
        SELECT actor_id
        FROM ap_remote_actor
        WHERE BTRIM(COALESCE(actor_id, '')) <> ''
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for actor_id in cached_actor_ids {
        if let Ok(parsed) = Url::parse(&actor_id) {
            if let Some(host) = parsed.host_str() {
                let _ = ensure_discovered_instance(pool, host, &actor_id).await;
            }
        }
    }

    sqlx::query(
        r#"
        DELETE FROM discovered_instance di
        WHERE NOT EXISTS (
            SELECT 1
            FROM ap_remote_actor ra
            WHERE LOWER(COALESCE(NULLIF(ra.host, ''), '')) = di.host
               OR ra.actor_id ILIKE ('https://' || di.host || '/%')
               OR ra.actor_id ILIKE ('http://' || di.host || '/%')
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}
