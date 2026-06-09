const FEED_PAGE_DEFAULT_LIMIT: i64 = 20;
const FEED_PAGE_MAX_LIMIT: i64 = 60;

const SEARCH_TERM_MAX_CHARS: usize = 80;
const SEARCH_USERS_LIMIT: i64 = 30;
const SEARCH_POSTS_LIMIT: i64 = 40;
const SEARCH_COMMUNITIES_LIMIT: i64 = 40;
const SEARCH_POST_BODY_PREVIEW_CHARS: usize = 220;

#[derive(Template)]
#[template(path = "search.html")]
#[allow(dead_code)]
pub struct SearchTemplate {
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
    pub searched: bool,
    pub search_term: String,
    pub users: Vec<SearchUserView>,
    pub posts: Vec<SearchPostView>,
}

#[derive(Deserialize)]
pub struct FeedPageQuery {
    pub before_post_id: Option<i64>,
    pub after_post_id: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub search: Option<String>,
}
pub async fn search(
    Query(query): Query<SearchQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;

    let search_term = query
        .search
        .as_deref()
        .map(normalize_search_term)
        .unwrap_or_default();
    let searched = !search_term.is_empty();

    let users = if searched {
        let mut users = load_search_users(&pool, &search_term).await;
        let discovered_remote_users =
            crate::federation::search_discovered_remote_users(&pool, &search_term).await;
        for remote_user in discovered_remote_users {
            let already_present = users
                .iter()
                .any(|user| user.handle.eq_ignore_ascii_case(&remote_user.handle));
            if !already_present {
                users.push(remote_user);
            }
        }
        if let Some(remote_user) =
            crate::federation::search_remote_user_by_handle(&pool, &search_term).await
        {
            let already_present = users
                .iter()
                .any(|user| user.handle.eq_ignore_ascii_case(&remote_user.handle));
            if !already_present {
                users.insert(0, remote_user);
            }
        }
        users
    } else {
        Vec::new()
    };
    let posts = if searched {
        let mut posts = load_search_posts(&pool, &search_term, current_user_id).await;
        posts.extend(crate::federation::search_discovered_remote_posts(&pool, &search_term).await);
        posts
    } else {
        Vec::new()
    };

    let template = SearchTemplate {
        title: "Search".to_string(),
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
        searched,
        search_term,
        users,
        posts,
    };

    render_template_response(&template)
}


#[derive(sqlx::FromRow)]
struct SearchUserRow {
    username: String,
    preferred_username: String,
    profile_photo_url: String,
    profile_photo_style: String,
}

#[derive(sqlx::FromRow)]
struct SearchPostRow {
    post_id: i64,
    author_username: String,
    author_profile_photo_url: String,
    author_profile_photo_style: String,
    body: String,
    link_url: String,
    first_image_url: String,
    created_at: String,
}


fn normalize_search_term(raw: &str) -> String {
    let collapsed = raw
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    collapsed.chars().take(SEARCH_TERM_MAX_CHARS).collect()
}


pub fn parse_local_username_lookup(raw: &str) -> Option<String> {
    let lookup = raw.trim();
    if lookup.is_empty() {
        return None;
    }

    if let Some((username_part, domain_part)) = lookup.split_once('@') {
        let username_part = username_part.trim();
        let domain_part = domain_part.trim();
        if username_part.is_empty() || domain_part.is_empty() {
            return None;
        }
        if !local_profile_domain_matches(domain_part) {
            return None;
        }
        return Some(username_part.to_string());
    }

    Some(lookup.to_string())
}

pub struct SearchUserView {
    pub preferred_username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub profile_url: String,
    pub handle: String,
    pub is_remote: bool,
}
async fn load_search_users(pool: &PgPool, term: &str) -> Vec<SearchUserView> {
    if term.is_empty() {
        return Vec::new();
    }

    let normalized_term = term.trim().trim_start_matches('@');
    let (plain_user_term, handle_term) =
        if let Some((username_part, domain_part)) = normalized_term.split_once('@') {
            if local_profile_domain_matches(domain_part) {
                (
                    username_part.trim().to_string(),
                    normalized_term.to_string(),
                )
            } else {
                (
                    "__instavox_no_local_user_match__".to_string(),
                    normalized_term.to_string(),
                )
            }
        } else {
            (normalized_term.to_string(), normalized_term.to_string())
        };
    let like_pattern = format!("%{}%", plain_user_term);
    let handle_like_pattern = format!("%{}%", handle_term);
    let local_domain = local_profile_domain();
    let rows = sqlx::query_as::<_, SearchUserRow>(
        r#"
        SELECT
            u.username,
            COALESCE(NULLIF(u.preferred_username, ''), '') AS preferred_username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS profile_photo_style
        FROM users u
        WHERE u.username ILIKE $1
           OR COALESCE(u.preferred_username, '') ILIKE $1
           OR LOWER(u.username || '@' || $4) LIKE LOWER($2)
           OR LOWER('@' || u.username || '@' || $4) LIKE LOWER($2)
        ORDER BY
            CASE
                WHEN LOWER(u.username) = LOWER($3) THEN 0
                WHEN LOWER(u.username || '@' || $4) = LOWER($5) THEN 1
                WHEN LOWER('@' || u.username || '@' || $4) = LOWER('@' || $5) THEN 2
                WHEN LOWER(COALESCE(u.preferred_username, '')) = LOWER($3) THEN 3
                ELSE 4
            END,
            LOWER(u.username) ASC,
            u.public_id ASC
        LIMIT $6
        "#,
    )
    .bind(like_pattern)
    .bind(handle_like_pattern)
    .bind(&plain_user_term)
    .bind(&local_domain)
    .bind(&handle_term)
    .bind(SEARCH_USERS_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| {
            let username = row.username;
            SearchUserView {
                profile_url: local_user_profile_path(&username),
                handle: format!("@{}@{}", username, local_profile_domain()),
                preferred_username: row.preferred_username,
                profile_photo_url: row.profile_photo_url,
                profile_photo_style: row.profile_photo_style,
                is_remote: false,
            }
        })
        .collect()
}

pub struct SearchPostView {
    pub author_username: String,
    pub author_handle: String,
    pub author_profile_url: String,
    pub author_profile_photo_url: String,
    pub author_profile_photo_style: String,
    pub body_preview: String,
    pub created_at: String,
    pub post_url: String,
    pub is_remote: bool,
}
async fn load_search_posts(
    pool: &PgPool,
    term: &str,
    current_user_id: Option<i32>,
) -> Vec<SearchPostView> {
    if term.is_empty() {
        return Vec::new();
    }

    let like_pattern = format!("%{}%", term);
    let rows = sqlx::query_as::<_, SearchPostRow>(
        r#"
        SELECT
            p.post_id,
            u.username AS author_username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS author_profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS author_profile_photo_style,
            COALESCE(NULLIF(p.body, ''), '') AS body,
            COALESCE(NULLIF(p.link_url, ''), '') AS link_url,
            COALESCE((
                SELECT pi.image_url
                FROM post_image pi
                WHERE pi.post_id = p.post_id
                ORDER BY pi.sort_order ASC, pi.image_id ASC
                LIMIT 1
            ), '') AS first_image_url,
            TO_CHAR(p.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
        FROM posts p
        JOIN users u ON u.id = p.user_id
        WHERE (
            COALESCE(p.body, '') ILIKE $1
            OR COALESCE(p.link_url, '') ILIKE $1
        )
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
        ORDER BY p.post_id DESC
        LIMIT $2
        "#,
    )
    .bind(like_pattern)
    .bind(SEARCH_POSTS_LIMIT)
    .bind(current_user_id.unwrap_or(0))
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| SearchPostView {
            author_username: row.author_username.clone(),
            author_handle: format!("@{}@{}", row.author_username, local_profile_domain()),
            author_profile_url: local_user_profile_path(&row.author_username),
            author_profile_photo_url: row.author_profile_photo_url,
            author_profile_photo_style: row.author_profile_photo_style,
            body_preview: build_post_preview(
                &row.body,
                &row.link_url,
                &row.first_image_url,
                SEARCH_POST_BODY_PREVIEW_CHARS,
            ),
            created_at: row.created_at,
            post_url: format!("/posts/{}", row.post_id),
            is_remote: false,
        })
        .collect()
}


#[derive(Serialize)]
pub struct FeedPageResponse {
    pub posts: Vec<IndexPostView>,
    pub has_more: bool,
    pub next_before_post_id: Option<i64>,
    pub next_after_post_id: Option<i64>,
}


#[derive(sqlx::FromRow)]
struct CommunitySearchRow {
    community_id: i64,
    slug: String,
    name: String,
    description: String,
    visibility: String,
    member_count: i64,
    post_count: i64,
    profile_photo_url: String,
    profile_photo_style: String,
    owner_user_id: Option<i32>,
    owner_username: String,
    created_at: String,
    //member_user_id: Option<i32>,
    //member_user_username: String,
}

fn map_community_row_to_view(row: CommunitySearchRow) -> CommunityPageView {
    CommunityPageView {
        community_id: row.community_id,
        slug: row.slug,
        name: row.name,
        description: row.description,
        visibility: row.visibility,
        member_count: row.member_count,
        post_count: row.post_count,
        profile_photo_url: row.profile_photo_url,
        profile_photo_style: row.profile_photo_style,
        owner_user_id: row.owner_user_id.unwrap_or(0),
        owner_username: row.owner_username,
        created_at: row.created_at,
    }
}

pub async fn load_community_by_slug(pool: &PgPool, slug: &str) -> Option<CommunityPageView> {
    let normalized_slug = normalize_community_slug(slug);
    if normalized_slug.is_empty() {
        return None;
    }

    let row = sqlx::query_as::<_, CommunitySearchRow>(
        r#"
        SELECT
            c.community_id,
            COALESCE(NULLIF(c.slug, ''), '') AS slug,
            COALESCE(NULLIF(c.name, ''), '') AS name,
            COALESCE(NULLIF(c.description, ''), '') AS description,
            LOWER(COALESCE(NULLIF(c.visibility, ''), 'public')) AS visibility,
            COALESCE(member_stats.member_count, 0) AS member_count,
            COALESCE(post_stats.post_count, 0) AS post_count,
            COALESCE(NULLIF(c.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(c.profile_photo_style, '') AS profile_photo_style,
            c.owner_user_id,
            COALESCE(owner.username, '') AS owner_username,
            TO_CHAR(c.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
        FROM community_page c
        LEFT JOIN users owner ON owner.id = c.owner_user_id
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::BIGINT AS member_count
            FROM community_member cm
            WHERE cm.community_id = c.community_id
        ) member_stats ON TRUE
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::BIGINT AS post_count
            FROM posts p
            WHERE p.community_id = c.community_id
        ) post_stats ON TRUE
        WHERE LOWER(COALESCE(c.status, 'active')) NOT IN ('deleted', 'banned')
          AND LOWER(c.slug) = LOWER($1)
        ORDER BY c.community_id DESC
        LIMIT 1
        "#,
    )
    .bind(normalized_slug)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    row.map(map_community_row_to_view)
}

pub async fn load_communities(
    pool: &PgPool,
    sort: &str,
    current_user_id: Option<i32>,
) -> Vec<CommunityPageView> {
    let safe_sort = normalize_community_sort(Some(sort));
    let viewer_user_id = current_user_id.unwrap_or(0);

    let query = match safe_sort {
        "new" => {
            r#"
            SELECT
                c.community_id,
                COALESCE(NULLIF(c.slug, ''), '') AS slug,
                COALESCE(NULLIF(c.name, ''), '') AS name,
                COALESCE(NULLIF(c.description, ''), '') AS description,
                LOWER(COALESCE(NULLIF(c.visibility, ''), 'public')) AS visibility,
                COALESCE(member_stats.member_count, 0) AS member_count,
                COALESCE(post_stats.post_count, 0) AS post_count,
                COALESCE(NULLIF(c.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                COALESCE(c.profile_photo_style, '') AS profile_photo_style,
                c.owner_user_id,
                COALESCE(owner.username, '') AS owner_username,
                TO_CHAR(c.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM community_page c
            LEFT JOIN users owner ON owner.id = c.owner_user_id
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS member_count
                FROM community_member cm
                WHERE cm.community_id = c.community_id
            ) member_stats ON TRUE
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS post_count
                FROM posts p
                WHERE p.community_id = c.community_id
            ) post_stats ON TRUE
            WHERE LOWER(COALESCE(c.status, 'active')) NOT IN ('deleted', 'banned')
              AND (
                  $2 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = c.community_id
                        AND ci.user_id = $2
                  )
              )
            ORDER BY c.community_id DESC
            LIMIT $1
            "#
        }
        "top" => {
            r#"
            SELECT
                c.community_id,
                COALESCE(NULLIF(c.slug, ''), '') AS slug,
                COALESCE(NULLIF(c.name, ''), '') AS name,
                COALESCE(NULLIF(c.description, ''), '') AS description,
                LOWER(COALESCE(NULLIF(c.visibility, ''), 'public')) AS visibility,
                COALESCE(member_stats.member_count, 0) AS member_count,
                COALESCE(post_stats.post_count, 0) AS post_count,
                COALESCE(NULLIF(c.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                COALESCE(c.profile_photo_style, '') AS profile_photo_style,
                c.owner_user_id,
                COALESCE(owner.username, '') AS owner_username,
                TO_CHAR(c.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM community_page c
            LEFT JOIN users owner ON owner.id = c.owner_user_id
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS member_count
                FROM community_member cm
                WHERE cm.community_id = c.community_id
            ) member_stats ON TRUE
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS post_count
                FROM posts p
                WHERE p.community_id = c.community_id
            ) post_stats ON TRUE
            WHERE LOWER(COALESCE(c.status, 'active')) NOT IN ('deleted', 'banned')
              AND (
                  $2 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = c.community_id
                        AND ci.user_id = $2
                  )
              )
            ORDER BY
                COALESCE(member_stats.member_count, 0) DESC,
                COALESCE(post_stats.post_count, 0) DESC,
                c.community_id DESC
            LIMIT $1
            "#
        }
        _ => {
            r#"
            SELECT
                c.community_id,
                COALESCE(NULLIF(c.slug, ''), '') AS slug,
                COALESCE(NULLIF(c.name, ''), '') AS name,
                COALESCE(NULLIF(c.description, ''), '') AS description,
                LOWER(COALESCE(NULLIF(c.visibility, ''), 'public')) AS visibility,
                COALESCE(member_stats.member_count, 0) AS member_count,
                COALESCE(post_stats.post_count, 0) AS post_count,
                COALESCE(NULLIF(c.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                COALESCE(c.profile_photo_style, '') AS profile_photo_style,
                c.owner_user_id,
                COALESCE(owner.username, '') AS owner_username,
                TO_CHAR(c.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
            FROM community_page c
            LEFT JOIN users owner ON owner.id = c.owner_user_id
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS member_count
                FROM community_member cm
                WHERE cm.community_id = c.community_id
            ) member_stats ON TRUE
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::BIGINT AS post_count
                FROM posts p
                WHERE p.community_id = c.community_id
            ) post_stats ON TRUE
            WHERE LOWER(COALESCE(c.status, 'active')) NOT IN ('deleted', 'banned')
              AND (
                  $2 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = c.community_id
                        AND ci.user_id = $2
                  )
              )
            ORDER BY
                (
                    COALESCE((
                        SELECT COUNT(*)::BIGINT
                        FROM posts p2
                        WHERE p2.community_id = c.community_id
                          AND p2.created_at >= NOW() - INTERVAL '7 days'
                    ), 0) * 6
                    + COALESCE(post_stats.post_count, 0) * 2
                    + COALESCE(member_stats.member_count, 0)
                ) DESC,
                c.community_id DESC
            LIMIT $1
            "#
        }
    };

    let rows = sqlx::query_as::<_, CommunitySearchRow>(query)
        .bind(SEARCH_COMMUNITIES_LIMIT)
        .bind(viewer_user_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    rows.into_iter().map(map_community_row_to_view).collect()
}

pub async fn load_joined_communities(pool: &PgPool, user_id: i32) -> Vec<CommunityPageView> {
    if user_id <= 0 {
        return Vec::new();
    }

    let rows = sqlx::query_as::<_, CommunitySearchRow>(
        r#"
        SELECT
            c.community_id,
            COALESCE(NULLIF(c.slug, ''), '') AS slug,
            COALESCE(NULLIF(c.name, ''), '') AS name,
            COALESCE(NULLIF(c.description, ''), '') AS description,
            LOWER(COALESCE(NULLIF(c.visibility, ''), 'public')) AS visibility,
            COALESCE(member_stats.member_count, 0) AS member_count,
            COALESCE(post_stats.post_count, 0) AS post_count,
            COALESCE(NULLIF(c.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
            COALESCE(c.profile_photo_style, '') AS profile_photo_style,
            c.owner_user_id,
            COALESCE(owner.username, '') AS owner_username,
            TO_CHAR(c.created_at, 'YYYY-MM-DD HH24:MI') AS created_at
        FROM community_member cm
        JOIN community_page c ON c.community_id = cm.community_id
        LEFT JOIN users owner ON owner.id = c.owner_user_id
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::BIGINT AS member_count
            FROM community_member cm2
            WHERE cm2.community_id = c.community_id
        ) member_stats ON TRUE
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::BIGINT AS post_count
            FROM posts p
            WHERE p.community_id = c.community_id
        ) post_stats ON TRUE
        WHERE cm.user_id = $1
          AND LOWER(COALESCE(c.status, 'active')) NOT IN ('deleted', 'banned')
          AND NOT EXISTS (
              SELECT 1
              FROM community_ignore ci
              WHERE ci.community_id = c.community_id
                AND ci.user_id = $1
          )
        ORDER BY cm.joined_at DESC, c.community_id DESC
        LIMIT $2
        "#,
    )
    .bind(user_id)
    .bind(SEARCH_COMMUNITIES_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter().map(map_community_row_to_view).collect()
}


pub fn is_fetch_request(headers: &HeaderMap) -> bool {
    headers
        .get("X-Requested-With")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("fetch"))
}