
pub async fn search_remote_user_by_handle(
    pool: &PgPool,
    search_term: &str,
) -> Option<crate::handler::SearchUserView> {
    let (username, domain) = parse_remote_search_handle(search_term)?;
    let discovery_status = get_instance_status(pool, &domain).await.ok()?;
    if discovery_status == "limited" || discovery_status == "ban" {
        return None;
    }

    if let Ok(Some(cached)) = load_cached_remote_actor_for_search(pool, &username, &domain).await {
        let cached_status = normalize_discovery_status(&cached.status);
        if cached_status == "limited" || cached_status == "ban" {
            return None;
        }
        return map_remote_search_view(cached);
    }

    let webfinger_url = format!(
        "https://{}/.well-known/webfinger?resource=acct:{}@{}",
        domain,
        urlencoding::encode(&username),
        urlencoding::encode(&domain)
    );
    let webfinger_response = remote_search_client()
        .get(&webfinger_url)
        .header(
            reqwest::header::ACCEPT,
            "application/jrd+json, application/json",
        )
        .send()
        .await
        .ok()?;
    if !webfinger_response.status().is_success() {
        return None;
    }

    let webfinger_json = webfinger_response.json::<Value>().await.ok()?;
    let actor_url = webfinger_json
        .get("links")
        .and_then(Value::as_array)
        .and_then(|links| {
            links.iter().find_map(|link| {
                let rel = link.get("rel").and_then(Value::as_str).unwrap_or("");
                let href = link
                    .get("href")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let link_type = link.get("type").and_then(Value::as_str).unwrap_or("");
                if rel.eq_ignore_ascii_case("self")
                    && !href.is_empty()
                    && (link_type.contains("activity+json")
                        || link_type.contains("ld+json")
                        || link_type.is_empty())
                {
                    Some(href.to_string())
                } else {
                    None
                }
            })
        })?;

    let actor_response = remote_search_client()
        .get(&actor_url)
        .header(
            reqwest::header::ACCEPT,
            "application/activity+json, application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\", application/json",
        )
        .send()
        .await
        .ok()?;
    if !actor_response.status().is_success() {
        return None;
    }

    let actor_json = actor_response.json::<Value>().await.ok()?;
    let remote_row = upsert_remote_actor_from_value(pool, &actor_url, &actor_json)
        .await
        .ok()?;
    map_remote_search_view(remote_row).or_else(|| {
        let remote = map_remote_search_user(
            RemoteActorRow {
                actor_id: actor_url,
                host: domain.clone(),
                preferred_username: username.clone(),
                display_name: String::new(),
                summary: String::new(),
                inbox_url: String::new(),
                shared_inbox_url: String::new(),
                outbox_url: String::new(),
                followers_url: String::new(),
                following_url: String::new(),
                public_key_pem: String::new(),
                icon_url: String::new(),
                status: "discovered".to_string(),
                discovered_at: None,
                last_refreshed_at: None,
            },
            &username,
            &domain,
        );
        Some(crate::handler::SearchUserView {
            preferred_username: remote.preferred_username,
            profile_photo_url: remote.profile_photo_url,
            profile_photo_style: remote.profile_photo_style,
            profile_url: remote.profile_url,
            handle: remote.handle,
            is_remote: remote.is_remote,
        })
    })
}

pub async fn search_discovered_remote_users(
    pool: &PgPool,
    search_term: &str,
) -> Vec<crate::handler::SearchUserView> {
    let normalized_term = search_term
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    if normalized_term.is_empty() {
        return Vec::new();
    }

    let like_pattern = format!("%{}%", normalized_term);
    let exact_lookup = parse_remote_search_handle(search_term);

    let rows = sqlx::query_as::<_, RemoteActorRow>(
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
        WHERE LOWER(COALESCE(NULLIF(status, ''), 'discovered')) = 'discovered'
          AND BTRIM(COALESCE(preferred_username, '')) <> ''
          AND (
                LOWER(preferred_username || '@' || host) LIKE $1
             OR LOWER('@' || preferred_username || '@' || host) LIKE $1
             OR LOWER(preferred_username) LIKE $1
             OR LOWER(display_name) LIKE $1
             OR LOWER(summary) LIKE $1
          )
        ORDER BY
            CASE
                WHEN LOWER(preferred_username) = LOWER($2)
                 AND LOWER(host) = LOWER($3)
                    THEN 0
                ELSE 1
            END,
            last_refreshed_at DESC NULLS LAST,
            actor_id ASC
        LIMIT 20
        "#,
    )
    .bind(&like_pattern)
    .bind(
        exact_lookup
            .as_ref()
            .map(|(username, _)| username.as_str())
            .unwrap_or(""),
    )
    .bind(
        exact_lookup
            .as_ref()
            .map(|(_, domain)| domain.as_str())
            .unwrap_or(""),
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .filter_map(map_remote_search_view)
        .collect()
}

pub async fn search_discovered_remote_posts(
    pool: &PgPool,
    search_term: &str,
) -> Vec<crate::handler::SearchPostView> {
    let normalized_term = search_term.trim().to_ascii_lowercase();
    if normalized_term.is_empty() {
        return Vec::new();
    }

    let like_pattern = format!("%{}%", normalized_term);
    let rows = sqlx::query_as::<_, RemotePostSearchRow>(
        r#"
        SELECT
            rp.object_id,
            COALESCE(NULLIF(rp.url, ''), rp.object_id) AS post_url,
            COALESCE(NULLIF(rp.content, ''), '') AS content,
            TO_CHAR(COALESCE(rp.published_at, rp.discovered_at), 'YYYY-MM-DD HH24:MI') AS created_at,
            ra.actor_id,
            ra.preferred_username,
            ra.display_name,
            ra.icon_url
        FROM ap_remote_post rp
        JOIN ap_remote_actor ra ON ra.actor_id = rp.actor_id
        LEFT JOIN discovered_instance di ON di.host = COALESCE(NULLIF(rp.host, ''), ra.host)
        WHERE LOWER(COALESCE(NULLIF(ra.status, ''), 'discovered')) = 'discovered'
          AND LOWER(COALESCE(NULLIF(di.status, ''), 'discovered')) = 'discovered'
          AND (
                LOWER(COALESCE(rp.content, '')) LIKE $1
             OR LOWER(COALESCE(rp.url, '')) LIKE $1
             OR LOWER(COALESCE(ra.preferred_username, '')) LIKE $1
             OR LOWER(COALESCE(ra.display_name, '')) LIKE $1
          )
        ORDER BY COALESCE(rp.published_at, rp.discovered_at) DESC, rp.object_id DESC
        LIMIT 40
        "#,
    )
    .bind(&like_pattern)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .filter_map(|row| {
            let actor_url = Url::parse(&row.actor_id).ok()?;
            let host = actor_url
                .host_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if host.is_empty() {
                return None;
            }
            let author_name = if row.display_name.trim().is_empty() {
                row.preferred_username.clone()
            } else {
                row.display_name.clone()
            };
            Some(crate::handler::SearchPostView {
                author_username: row.preferred_username.clone(),
                author_handle: format!("@{}@{}", row.preferred_username, host),
                author_profile_url: format!("/user/{}@{}", row.preferred_username, host),
                author_profile_photo_url: if row.icon_url.trim().is_empty() {
                    DEFAULT_PROFILE_PHOTO_URL.to_string()
                } else {
                    row.icon_url
                },
                author_profile_photo_style: String::new(),
                body_preview: truncate_search_preview(
                    if row.content.trim().is_empty() {
                        &row.post_url
                    } else {
                        &row.content
                    },
                    220,
                ),
                created_at: row.created_at,
                post_url: if row.post_url.trim().is_empty() {
                    row.object_id
                } else {
                    row.post_url
                },
                is_remote: true,
            })
            .map(|mut post| {
                if !author_name.trim().is_empty() {
                    post.author_username = author_name;
                }
                post
            })
        })
        .collect()
}
