use askama::Template;
pub use serde::Serialize;
use sqlx::FromRow;
pub use sqlx::PgPool;

use crate::handlers::notifications::HeaderNotificationView;


pub const MAX_SETTINGS_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_POST_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_POST_IMAGES: usize = 8;
pub const MAX_POST_TEXT_LENGTH: usize = 10_000;
pub const MAX_POST_COMMENT_LENGTH: usize = 1_000;

pub const LINK_PREVIEW_MAX_HTML_BYTES: usize = 512 * 1024;
pub const LINK_PREVIEW_TIMEOUT_SECS: u64 = 6;

pub const POST_VISIBILITY_PUBLIC: &str = "public";
pub const POST_VISIBILITY_FOLLOWING: &str = "following";
pub const POST_VISIBILITY_FRIENDS: &str = "friends";
pub const POST_VISIBILITY_PRIVATE: &str = "private";

pub const FEED_PAGE_DEFAULT_LIMIT: i64 = 20;
pub const FEED_PAGE_MAX_LIMIT: i64 = 50;



#[derive(Serialize)]
pub struct IndexPostView {
    pub post_id: i64,
    pub author_public_id: i64,
    pub author_username: String,
    pub author_profile_photo_url: String,
    pub author_profile_photo_style: String,
    pub body: String,
    pub link_url: String,
    pub visibility: String,
    pub visibility_label: String,
    pub community_name: String,
    pub community_slug: String,
    pub link_title: String,
    pub link_description: String,
    pub link_image_url: String,
    pub has_link_preview: bool,
    pub image_urls: Vec<String>,
    pub likes_count: i64,
    pub dislikes_count: i64,
    pub comments_count: i64,
    pub shares_count: i64,
    pub liked_by_current_user: bool,
    pub disliked_by_current_user: bool,
    pub shared_by_current_user: bool,
    pub comments: Vec<PostCommentView>,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
pub struct FeedPostRow {
    post_id: i64,
    author_public_id: i64,
    author_username: String,
    author_profile_photo_url: String,
    author_profile_photo_style: String,
    body: String,
    link_url: String,
    visibility: String,
    community_name: String,
    community_slug: String,
    created_at: String,
}

#[derive(sqlx::FromRow)]
pub struct FeedPostImageRow {
    post_id: i64,
    image_url: String,
}

pub struct SavedPostImage {
    image_url: String,
    mime_type: String,
}

#[derive(Template)]
#[template(path = "models/post.html")]
#[allow(dead_code)]
pub struct PostTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub post_found: bool,
    pub post: IndexPostView,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
}


pub async fn ensure_post_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS posts (
            post_id BIGSERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            body TEXT NOT NULL DEFAULT '',
            link_url TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE posts
        ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'public'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE posts
        SET visibility = 'public'
        WHERE LOWER(COALESCE(visibility, '')) NOT IN ('public', 'following', 'friends', 'private')
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE posts
        DROP CONSTRAINT IF EXISTS posts_visibility_chk
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE posts
        ADD CONSTRAINT posts_visibility_chk
        CHECK (LOWER(COALESCE(visibility, '')) IN ('public', 'following', 'friends', 'private'))
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS posts_user_created_idx
        ON posts (user_id, created_at DESC, post_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_image (
            image_id BIGSERIAL PRIMARY KEY,
            post_id BIGINT NOT NULL REFERENCES posts(post_id) ON DELETE CASCADE,
            image_url TEXT NOT NULL,
            mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_image_post_idx
        ON post_image (post_id, sort_order, image_id)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_like (
            post_id BIGINT NOT NULL REFERENCES posts(post_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (post_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_like_user_idx
        ON post_like (user_id, created_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_dislike (
            post_id BIGINT NOT NULL REFERENCES posts(post_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (post_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_dislike_user_idx
        ON post_dislike (user_id, created_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        DELETE FROM post_dislike d
        WHERE EXISTS (
            SELECT 1
            FROM post_like l
            WHERE l.post_id = d.post_id
              AND l.user_id = d.user_id
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_share (
            post_id BIGINT NOT NULL REFERENCES posts(post_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (post_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_share_user_idx
        ON post_share (user_id, created_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_comment (
            comment_id BIGSERIAL PRIMARY KEY,
            post_id BIGINT NOT NULL REFERENCES posts(post_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            body TEXT NOT NULL DEFAULT '',
            reply_to_comment_id BIGINT REFERENCES post_comment(comment_id) ON DELETE SET NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE post_comment
        ADD COLUMN IF NOT EXISTS reply_to_comment_id BIGINT
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_comment_reply_idx
        ON post_comment (reply_to_comment_id, comment_id)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_comment_post_idx
        ON post_comment (post_id, created_at ASC, comment_id ASC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS post_comment_like (
            comment_id BIGINT NOT NULL REFERENCES post_comment(comment_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (comment_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS post_comment_like_user_idx
        ON post_comment_like (user_id, created_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE OR REPLACE FUNCTION can_view_post(
            post_owner_id INTEGER,
            post_visibility TEXT,
            viewer_id INTEGER
        )
        RETURNS BOOLEAN
        LANGUAGE SQL
        STABLE
        AS $$
        SELECT CASE LOWER(COALESCE(post_visibility, 'public'))
            WHEN 'public' THEN TRUE
            WHEN 'private' THEN viewer_id > 0 AND post_owner_id = viewer_id
            WHEN 'friends' THEN viewer_id > 0
                AND (
                    post_owner_id = viewer_id
                    OR EXISTS(
                        SELECT 1
                        FROM relationship r
                        WHERE (
                            (r.sender_id = post_owner_id AND r.receiver_id = viewer_id)
                            OR (r.sender_id = viewer_id AND r.receiver_id = post_owner_id)
                        )
                          AND LOWER(COALESCE(r.status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
                    )
                )
            WHEN 'following' THEN viewer_id > 0
                AND (
                    post_owner_id = viewer_id
                    OR EXISTS(
                        SELECT 1
                        FROM relationship r
                        WHERE (
                                (r.sender_id = viewer_id AND r.receiver_id = post_owner_id)
                                OR
                                (r.sender_id = post_owner_id AND r.receiver_id = viewer_id)
                              )
                          AND LOWER(COALESCE(r.status, '')) IN (
                              'follow',
                              'following',
                              'follower'
                          )
                    )
                    OR EXISTS(
                        SELECT 1
                        FROM relationship r
                        WHERE (
                            (r.sender_id = post_owner_id AND r.receiver_id = viewer_id)
                            OR (r.sender_id = viewer_id AND r.receiver_id = post_owner_id)
                        )
                          AND LOWER(COALESCE(r.status, '')) IN (
                              'friend',
                              'friends',
                              'friendship',
                              'accepted'
                          )
                    )
                )
            ELSE TRUE
        END
        $$;
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}


pub async fn read_post_draft_from_multipart(
    mut multipart: Multipart,
    post_owner_user_id: i32,
    forced_visibility: Option<&str>,
) -> Result<PendingPostDraft, (StatusCode, String)> {
    let mut text = String::new();
    let mut post_visibility = forced_visibility
        .unwrap_or(POST_VISIBILITY_PUBLIC)
        .to_string();
    let mut saved_images: Vec<SavedPostImage> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid post payload".to_string()))?
    {
        let field_name = field.name().unwrap_or("").to_string();

        if field_name == "text" {
            text = field.text().await.unwrap_or_default();
            continue;
        }

        if field_name == "visibility" && forced_visibility.is_none() {
            let requested_visibility = field.text().await.unwrap_or_default();
            post_visibility = normalize_post_visibility(&requested_visibility).to_string();
            continue;
        }

        if field_name != "images" {
            continue;
        }

        if saved_images.len() >= MAX_POST_IMAGES {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("You can upload up to {} images per post", MAX_POST_IMAGES),
            ));
        }

        let file_name = field.file_name().map(str::to_string);
        let content_type = field.content_type().map(str::to_string);
        let bytes = field.bytes().await.map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Failed to read uploaded image".to_string(),
            )
        })?;

        if bytes.is_empty() {
            continue;
        }

        let saved_image = save_post_image_file(
            post_owner_user_id,
            file_name.as_deref(),
            content_type.as_deref(),
            &bytes,
        )
        .await
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
        saved_images.push(saved_image);
    }

    let text = text.trim().to_string();
    if text.len() > MAX_POST_TEXT_LENGTH {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Post text must be at most {} characters",
                MAX_POST_TEXT_LENGTH
            ),
        ));
    }

    if text.is_empty() && saved_images.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "A post requires text or at least one image".to_string(),
        ));
    }

    Ok(PendingPostDraft {
        text,
        visibility: post_visibility,
        saved_images,
    })
}


#[derive(sqlx::FromRow)]
pub struct PostOwnerRow {
    pub user_id: i32,
}


struct PendingPostDraft {
    text: String,
    visibility: String,
    saved_images: Vec<SavedPostImage>,
}

pub async fn insert_post_from_draft(
    pool: &PgPool,
    author_user_id: i32,
    community_id: Option<i64>,
    draft: PendingPostDraft,
) -> Result<i64, String> {
    let detected_link = extract_first_link_from_text(&draft.text).unwrap_or_default();
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| format!("post transaction begin failed: {}", err))?;

    let post_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO posts (user_id, body, link_url, visibility, community_id, created_at, updated_at)
        VALUES ($1, $2, NULLIF($3, ''), $4, $5, NOW(), NOW())
        RETURNING post_id
        "#,
    )
    .bind(author_user_id)
    .bind(&draft.text)
    .bind(&detected_link)
    .bind(&draft.visibility)
    .bind(community_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| format!("post insert failed: {}", err))?;

    for (index, image) in draft.saved_images.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO post_image (post_id, image_url, mime_type, sort_order, created_at)
            VALUES ($1, $2, $3, $4, NOW())
            "#,
        )
        .bind(post_id)
        .bind(&image.image_url)
        .bind(&image.mime_type)
        .bind(index as i32)
        .execute(&mut *tx)
        .await
        .map_err(|err| format!("post image insert failed: {}", err))?;
    }

    tx.commit()0
        .await
        .map_err(|err| format!("post commit failed: {}", err))?;

    Ok(post_id)
}


pub async fn post_detail(
    Path(post_id): Path<i64>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_user_id_value = current_user_id.unwrap_or(0);
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let is_moderator = match current_user_id {
        Some(user_id) => is_moderator_role(&load_user_role(&pool, user_id).await),
        None => false,
    };
    let loaded_post = if is_moderator {
        load_single_post_unrestricted(&pool, post_id, current_user_id).await
    } else {
        load_single_post(&pool, post_id, current_user_id).await
    };
    let post_found = loaded_post.is_some();
    let post = loaded_post.unwrap_or_else(|| empty_index_post_view(post_id));

    let template = PostTemplate {
        title: if post_found {
            format!("Post {} - Instavox", post_id)
        } else {
            "Post not found - Instavox".to_string()
        },
        id: current_public_id,
        user_id: current_user_id_value,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        post_found,
        post,
        unread_notifications_count,
        notifications,
    };

    render_template_response(&template)
}


#[derive(Serialize)]
pub struct IndexPostView {
    pub post_id: i64,
    pub author_public_id: i64,
    pub author_username: String,
    pub author_profile_photo_url: String,
    pub author_profile_photo_style: String,
    pub body: String,
    pub link_url: String,
    pub visibility: String,
    pub visibility_label: String,
    pub community_name: String,
    pub community_slug: String,
    pub link_title: String,
    pub link_description: String,
    pub link_image_url: String,
    pub has_link_preview: bool,
    pub image_urls: Vec<String>,
    pub likes_count: i64,
    pub dislikes_count: i64,
    pub comments_count: i64,
    pub shares_count: i64,
    pub liked_by_current_user: bool,
    pub disliked_by_current_user: bool,
    pub shared_by_current_user: bool,
    pub comments: Vec<PostCommentView>,
    pub created_at: String,
}


#[derive(Serialize, Clone)]
pub struct PostCommentView {
    pub comment_id: i64,
    pub commenter_public_id: i64,
    pub commenter_username: String,
    pub commenter_profile_photo_url: String,
    pub commenter_profile_photo_style: String,
    pub body: String,
    pub created_at: String,
    pub likes_count: i64,
    pub liked_by_current_user: bool,
    pub reply_parent_comment_id: i64,
    pub reply_to_comment_id: Option<i64>,
    pub reply_to_body_preview: String,
    pub reply_to_username: String,
    pub thread_depth: i64,
    pub thread_root_comment_id: i64,
}


#[derive(Serialize)]
pub struct FeedPageResponse {
    pub posts: Vec<IndexPostView>,
    pub has_more: bool,
    pub next_before_post_id: Option<i64>,
    pub next_after_post_id: Option<i64>,
}


#[derive(FromRow)]
struct FeedPostRow {
    post_id: i64,
    author_public_id: i64,
    author_username: String,
    author_profile_photo_url: String,
    author_profile_photo_style: String,
    body: String,
    link_url: String,
    visibility: String,
    community_name: String,
    community_slug: String,
    created_at: String,
}

pub fn normalize_feed_limit(limit: Option<i64>) -> i64 {
    limit
        .unwrap_or(FEED_PAGE_DEFAULT_LIMIT)
        .clamp(1, FEED_PAGE_MAX_LIMIT)
}


pub async fn build_feed_post_views(
    _pool: &PgPool,
    post_rows: Vec<FeedPostRow>,
    _current_user_id: Option<i32>,
) -> Vec<IndexPostView> {
    post_rows
        .into_iter()
        .map(|row| {
            let visibility_label = match row.visibility.as_str() {
                "followers" => "Followers",
                "private" => "Private",
                "unlisted" => "Unlisted",
                _ => "Public",
            }
            .to_string();

            IndexPostView {
                post_id: row.post_id,
                author_public_id: row.author_public_id,
                author_username: row.author_username,
                author_profile_photo_url: row.author_profile_photo_url,
                author_profile_photo_style: row.author_profile_photo_style,
                body: row.body,
                link_url: row.link_url,
                visibility: row.visibility,
                visibility_label,
                community_name: row.community_name,
                community_slug: row.community_slug,
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
                created_at: row.created_at,
            }
        })
        .collect()
}


pub async fn load_index_posts_segment(
    pool: &PgPool,
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
            WHERE p.post_id > $1
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $3
              )
              AND (
                  p.community_id IS NULL
                  OR $3 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = p.community_id
                        AND ci.user_id = $3
                  )
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
            ORDER BY p.created_at ASC, p.post_id ASC
            LIMIT $2
            "#,
        )
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
            WHERE p.post_id < $1
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $3
              )
              AND (
                  p.community_id IS NULL
                  OR $3 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = p.community_id
                        AND ci.user_id = $3
                  )
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
            WHERE can_view_post(
                p.user_id,
                COALESCE(NULLIF(p.visibility, ''), 'public'),
                $2
            )
              AND (
                  p.community_id IS NULL
                  OR $2 <= 0
                  OR NOT EXISTS (
                      SELECT 1
                      FROM community_ignore ci
                      WHERE ci.community_id = p.community_id
                        AND ci.user_id = $2
                  )
              )
              AND (
                  p.community_id IS NULL
                  OR p.user_id = $2
                  OR (
                      $2 > 0
                      AND EXISTS (
                          SELECT 1
                          FROM community_member cm
                          WHERE cm.community_id = p.community_id
                            AND cm.user_id = $2
                      )
                  )
              )
            ORDER BY p.created_at DESC, p.post_id DESC
            LIMIT $1
            "#,
        )
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


pub async fn load_index_posts(pool: &PgPool, current_user_id: Option<i32>) -> Vec<IndexPostView> {
    load_index_posts_segment(pool, current_user_id, None, None, FEED_PAGE_MAX_LIMIT)
        .await
        .posts
}


pub async fn load_single_post(
    pool: &PgPool,
    post_id: i64,
    current_user_id: Option<i32>,
) -> Option<IndexPostView> {
    let viewer_user_id = current_user_id.unwrap_or(0);
    let row = sqlx::query_as::<_, FeedPostRow>(
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
        WHERE p.post_id = $1
          AND can_view_post(
              p.user_id,
              COALESCE(NULLIF(p.visibility, ''), 'public'),
              $2
          )
          AND (
              p.community_id IS NULL
              OR p.user_id = $2
              OR EXISTS (
                  SELECT 1
                  FROM community_page cp
                  WHERE cp.community_id = p.community_id
                    AND LOWER(COALESCE(cp.visibility, 'public')) <> 'private'
              )
              OR (
                  $2 > 0
                  AND EXISTS (
                      SELECT 1
                      FROM community_member cm
                      WHERE cm.community_id = p.community_id
                        AND cm.user_id = $2
                  )
              )
          )
        "#,
    )
    .bind(post_id)
    .bind(viewer_user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    let mut posts = build_feed_post_views(pool, vec![row], current_user_id).await;
    posts.pop()
}

pub async fn load_single_post_unrestricted(
    pool: &PgPool,
    post_id: i64,
    current_user_id: Option<i32>,
) -> Option<IndexPostView> {
    let row = sqlx::query_as::<_, FeedPostRow>(
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
        WHERE p.post_id = $1
        "#,
    )
    .bind(post_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    let mut posts = build_feed_post_views(pool, vec![row], current_user_id).await;
    posts.pop()
}


pub fn normalize_feed_limit(raw_limit: Option<i64>) -> i64 {
    raw_limit
        .unwrap_or(FEED_PAGE_DEFAULT_LIMIT)
        .clamp(1, FEED_PAGE_MAX_LIMIT)
}

pub async fn build_feed_post_views(
    pool: &PgPool,
    post_rows: Vec<FeedPostRow>,
    current_user_id: Option<i32>,
) -> Vec<IndexPostView> {
    let post_ids: Vec<i64> = post_rows.iter().map(|row| row.post_id).collect();
    let mut images_by_post = load_post_images_by_post(pool, &post_ids).await;
    let mut interactions_by_post =
        load_post_interactions_by_post(pool, &post_ids, current_user_id).await;
    let mut comments_by_post = load_post_comments_by_post(pool, &post_ids, current_user_id).await;

    let mut posts = Vec::with_capacity(post_rows.len());
    for row in post_rows {
        let trimmed_link = row.link_url.trim().to_string();
        let normalized_visibility = normalize_post_visibility(&row.visibility).to_string();
        let community_name = row.community_name.trim().to_string();
        let community_slug = row.community_slug.trim().to_string();
        let visibility_label = if community_name.is_empty() || community_slug.is_empty() {
            post_visibility_label(&normalized_visibility).to_string()
        } else {
            community_name.clone()
        };
        let has_link_preview = !trimmed_link.is_empty();
        let (link_title, link_description, link_image_url) = if has_link_preview {
            build_link_preview_data(&trimmed_link).await
        } else {
            (String::new(), String::new(), String::new())
        };

        let interaction = interactions_by_post.remove(&row.post_id);
        posts.push(IndexPostView {
            post_id: row.post_id,
            author_public_id: row.author_public_id,
            author_username: row.author_username,
            author_profile_photo_url: row.author_profile_photo_url,
            author_profile_photo_style: row.author_profile_photo_style,
            body: row.body,
            link_url: trimmed_link,
            visibility: normalized_visibility.clone(),
            visibility_label,
            community_name,
            community_slug,
            link_title,
            link_description,
            link_image_url,
            has_link_preview,
            image_urls: images_by_post.remove(&row.post_id).unwrap_or_default(),
            likes_count: interaction
                .as_ref()
                .map(|item| item.likes_count)
                .unwrap_or(0),
            dislikes_count: interaction
                .as_ref()
                .map(|item| item.dislikes_count)
                .unwrap_or(0),
            comments_count: interaction
                .as_ref()
                .map(|item| item.comments_count)
                .unwrap_or(0),
            shares_count: interaction
                .as_ref()
                .map(|item| item.shares_count)
                .unwrap_or(0),
            liked_by_current_user: interaction
                .as_ref()
                .map(|item| item.liked_by_current_user)
                .unwrap_or(false),
            disliked_by_current_user: interaction
                .as_ref()
                .map(|item| item.disliked_by_current_user)
                .unwrap_or(false),
            shared_by_current_user: interaction
                .as_ref()
                .map(|item| item.shared_by_current_user)
                .unwrap_or(false),
            comments: comments_by_post.remove(&row.post_id).unwrap_or_default(),
            created_at: row.created_at,
        });
    }

    posts
}


pub fn sanitize_post_filename(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            cleaned.push(ch);
        } else {
            cleaned.push('_');
        }
    }
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        "image".to_string()
    } else {
        cleaned
    }
}

pub fn detect_post_image_extension(
    file_name: Option<&str>,
    content_type: Option<&str>,
) -> Option<String> {
    let from_mime = match content_type {
        Some("image/jpeg") | Some("image/jpg") => Some("jpg"),
        Some("image/png") => Some("png"),
        Some("image/gif") => Some("gif"),
        Some("image/webp") => Some("webp"),
        Some("image/avif") => Some("avif"),
        Some("image/bmp") => Some("bmp"),
        _ => None,
    };
    if let Some(ext) = from_mime {
        return Some(ext.to_string());
    }

    let ext = file_name
        .and_then(|name| FsPath::new(name).extension())
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    match ext.as_deref() {
        Some("jpg") | Some("jpeg") => Some("jpg".to_string()),
        Some("png") => Some("png".to_string()),
        Some("gif") => Some("gif".to_string()),
        Some("webp") => Some("webp".to_string()),
        Some("avif") => Some("avif".to_string()),
        Some("bmp") => Some("bmp".to_string()),
        _ => None,
    }
}

pub fn is_supported_post_link(link_url: &str) -> bool {
    if link_url.trim().is_empty() {
        return true;
    }
    let Ok(parsed) = Url::parse(link_url.trim()) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
}

pub fn extract_first_link_from_text(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let candidate = token
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '(' | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | '"'
                        | '\''
                        | ','
                        | '.'
                        | '!'
                        | '?'
                        | ';'
                        | ':'
                )
            })
            .to_string();
        if candidate.is_empty() {
            continue;
        }
        if is_supported_post_link(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub fn link_preview_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(Duration::from_secs(LINK_PREVIEW_TIMEOUT_SECS))
            .user_agent("Instavox/0.1 link-preview")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub fn link_url_has_image_extension(parsed: &Url) -> bool {
    let extension = parsed
        .path()
        .rsplit('.')
        .next()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        extension.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "bmp"
    )
}

pub fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub fn extract_attribute_value(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();

    let double_pattern = format!("{}=\"", attr);
    if let Some(start) = lower.find(&double_pattern) {
        let from = start + double_pattern.len();
        if let Some(end_rel) = tag[from..].find('"') {
            return Some(tag[from..from + end_rel].to_string());
        }
    }

    let single_pattern = format!("{}='", attr);
    if let Some(start) = lower.find(&single_pattern) {
        let from = start + single_pattern.len();
        if let Some(end_rel) = tag[from..].find('\'') {
            return Some(tag[from..from + end_rel].to_string());
        }
    }

    let bare_pattern = format!("{}=", attr);
    if let Some(start) = lower.find(&bare_pattern) {
        let from = start + bare_pattern.len();
        let end_rel = tag[from..]
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '>')
            .unwrap_or(tag.len().saturating_sub(from));
        let value = tag[from..from + end_rel]
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }

    None
}

pub fn normalize_link_image_url(base_url: &Url, candidate: &str) -> Option<String> {
    let decoded = decode_html_entities(candidate.trim());
    if decoded.is_empty() {
        return None;
    }

    let lowered = decoded.to_ascii_lowercase();
    if lowered.starts_with("data:") || lowered.starts_with("javascript:") {
        return None;
    }

    let absolute_url = if decoded.starts_with("//") {
        format!("{}:{}", base_url.scheme(), decoded)
    } else if let Ok(parsed) = Url::parse(&decoded) {
        parsed.to_string()
    } else if let Ok(joined) = base_url.join(&decoded) {
        joined.to_string()
    } else {
        return None;
    };

    let parsed = Url::parse(&absolute_url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.to_string())
}

#[derive(Clone)]
struct LinkImageCandidate {
    image_url: String,
    score: i32,
}

fn has_any_marker(haystack: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| haystack.contains(marker))
}

fn parse_dimension(value: &str) -> Option<i32> {
    let digits: String = value.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i32>().ok().filter(|value| *value > 0)
}

fn choose_best_srcset_candidate(srcset: &str) -> Option<String> {
    let mut best_url = String::new();
    let mut best_weight = 0f32;

    for entry in srcset.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let url = parts.next().unwrap_or("").trim();
        if url.is_empty() {
            continue;
        }

        let descriptor = parts.next().unwrap_or("").trim();
        let mut weight = 1f32;
        if let Some(width) = descriptor.strip_suffix('w') {
            weight = width.parse::<f32>().ok().unwrap_or(1f32);
        } else if let Some(scale) = descriptor.strip_suffix('x') {
            weight = scale.parse::<f32>().ok().unwrap_or(1f32) * 1000f32;
        }

        if weight >= best_weight {
            best_weight = weight;
            best_url = url.to_string();
        }
    }

    if best_url.is_empty() {
        None
    } else {
        Some(best_url)
    }
}

fn extract_img_source_candidate(tag: &str) -> Option<String> {
    for attr in [
        "data-src",
        "data-lazy-src",
        "data-original",
        "srcset",
        "data-srcset",
        "src",
    ] {
        let Some(value) = extract_attribute_value(tag, attr) else {
            continue;
        };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }

        if attr.ends_with("srcset") {
            if let Some(best_srcset_url) = choose_best_srcset_candidate(trimmed) {
                return Some(best_srcset_url);
            }
            continue;
        }

        return Some(trimmed.to_string());
    }
    None
}

fn score_image_candidate(tag: &str, image_url: &str, source_bias: i32) -> i32 {
    let lower_tag = tag.to_ascii_lowercase();
    let lower_url = image_url.to_ascii_lowercase();
    let combined = format!("{} {}", lower_tag, lower_url);

    let mut score = source_bias;

    if let Ok(parsed) = Url::parse(image_url) {
        let ext = parsed
            .path()
            .rsplit('.')
            .next()
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "bmp" => score += 10,
            "svg" => score -= 55,
            _ => {}
        }
    }

    if has_any_marker(
        &combined,
        &[
            "logo",
            "favicon",
            "icon",
            "avatar",
            "brandmark",
            "sprite",
            "header-logo",
            "nav-logo",
            "navbar",
            "site-header",
            "apple-touch-icon",
            "mask-icon",
            "placeholder",
            "spacer",
            "pixel",
        ],
    ) {
        score -= 70;
    }

    if has_any_marker(
        &combined,
        &[
            "hero",
            "featured",
            "feature-image",
            "cover",
            "article",
            "blog",
            "post",
            "story",
            "content-image",
        ],
    ) {
        score += 18;
    }

    let width = extract_attribute_value(tag, "width")
        .as_deref()
        .and_then(parse_dimension);
    let height = extract_attribute_value(tag, "height")
        .as_deref()
        .and_then(parse_dimension);

    if let (Some(width), Some(height)) = (width, height) {
        if width < 120 || height < 120 || width.saturating_mul(height) <= 35_000 {
            score -= 35;
        }
        if width >= 400 && height >= 220 {
            score += 12;
        }
    } else if width.is_some_and(|value| value < 120) || height.is_some_and(|value| value < 120) {
        score -= 25;
    }

    if lower_tag.contains("display:none") || lower_tag.contains("visibility:hidden") {
        score -= 25;
    }

    score
}

fn extract_meta_image_candidates_from_html(html: &str, base_url: &Url) -> Vec<LinkImageCandidate> {
    let mut candidates = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0usize;

    while let Some(start_rel) = lower[cursor..].find("<meta") {
        let start = cursor + start_rel;
        let rest_lower = &lower[start..];
        let Some(end_rel) = rest_lower.find('>') else {
            break;
        };
        let tag = &html[start..=start + end_rel];

        let key = extract_attribute_value(tag, "property")
            .or_else(|| extract_attribute_value(tag, "name"))
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default();

        let is_supported_image_key = matches!(
            key.as_str(),
            "og:image"
                | "og:image:url"
                | "og:image:secure_url"
                | "twitter:image"
                | "twitter:image:src"
                | "twitter:image:url"
        );

        if is_supported_image_key {
            if let Some(content) = extract_attribute_value(tag, "content") {
                if let Some(image_url) = normalize_link_image_url(base_url, &content) {
                    candidates.push(LinkImageCandidate {
                        image_url: image_url.clone(),
                        score: score_image_candidate(tag, &image_url, 72),
                    });
                }
            }
        }

        cursor = start + end_rel + 1;
    }

    candidates
}

fn extract_img_candidates_from_html(
    html: &str,
    base_url: &Url,
    source_bias: i32,
) -> Vec<LinkImageCandidate> {
    let mut candidates = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0usize;

    while let Some(start_rel) = lower[cursor..].find("<img") {
        let start = cursor + start_rel;
        let rest_lower = &lower[start..];
        let Some(end_rel) = rest_lower.find('>') else {
            break;
        };
        let tag = &html[start..=start + end_rel];

        if let Some(source) = extract_img_source_candidate(tag) {
            if let Some(image_url) = normalize_link_image_url(base_url, &source) {
                candidates.push(LinkImageCandidate {
                    image_url: image_url.clone(),
                    score: score_image_candidate(tag, &image_url, source_bias),
                });
            }
        }

        cursor = start + end_rel + 1;
    }

    candidates
}

fn extract_section_html_fragments<'a>(html: &'a str, section_tag: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open_token = format!("<{}", section_tag);
    let close_token = format!("</{}>", section_tag);

    let mut sections = Vec::new();
    let mut cursor = 0usize;

    while let Some(start_rel) = lower[cursor..].find(&open_token) {
        let start = cursor + start_rel;
        let Some(open_end_rel) = lower[start..].find('>') else {
            break;
        };
        let content_start = start + open_end_rel + 1;
        let Some(close_rel) = lower[content_start..].find(&close_token) else {
            break;
        };
        let content_end = content_start + close_rel;
        sections.push(&html[content_start..content_end]);
        cursor = content_end + close_token.len();
    }

    sections
}

fn pick_best_link_image(candidates: Vec<LinkImageCandidate>) -> Option<String> {
    let mut best_by_url = BTreeMap::<String, i32>::new();
    for candidate in candidates {
        if candidate.image_url.is_empty() {
            continue;
        }
        best_by_url
            .entry(candidate.image_url)
            .and_modify(|score| *score = (*score).max(candidate.score))
            .or_insert(candidate.score);
    }

    let mut best: Option<(String, i32)> = None;
    for (image_url, score) in best_by_url {
        if best
            .as_ref()
            .is_none_or(|(_, best_score)| score > *best_score)
        {
            best = Some((image_url, score));
        }
    }

    let (image_url, score) = best?;
    if score < 8 {
        return None;
    }
    Some(image_url)
}

fn extract_best_link_image_from_html(html: &str, base_url: &Url) -> Option<String> {
    let mut candidates = extract_meta_image_candidates_from_html(html, base_url);

    for section in ["main", "article"] {
        for section_html in extract_section_html_fragments(html, section) {
            candidates.extend(extract_img_candidates_from_html(section_html, base_url, 48));
        }
    }

    candidates.extend(extract_img_candidates_from_html(html, base_url, 28));
    pick_best_link_image(candidates)
}

async fn fetch_link_html(link_url: &str) -> Option<String> {
    let response = link_preview_client().get(link_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.contains("text/html") && !content_type.contains("application/xhtml+xml") {
        return None;
    }

    let bytes = response.bytes().await.ok()?;
    let html_bytes: &[u8] = if bytes.len() > LINK_PREVIEW_MAX_HTML_BYTES {
        &bytes[..LINK_PREVIEW_MAX_HTML_BYTES]
    } else {
        bytes.as_ref()
    };

    Some(String::from_utf8_lossy(html_bytes).to_string())
}

pub async fn resolve_first_image_from_link(link_url: &str) -> String {
    let trimmed = link_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let Ok(parsed) = Url::parse(trimmed) else {
        return String::new();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return String::new();
    }

    if link_url_has_image_extension(&parsed) {
        return trimmed.to_string();
    }

    let Some(html) = fetch_link_html(trimmed).await else {
        return String::new();
    };

    if let Some(image_url) = extract_best_link_image_from_html(&html, &parsed) {
        return image_url;
    }

    String::new()
}

async fn build_link_preview_data(link_url: &str) -> (String, String, String) {
    let trimmed = link_url.trim();
    if trimmed.is_empty() {
        return (String::new(), String::new(), String::new());
    }

    let Ok(parsed) = Url::parse(trimmed) else {
        return (
            "External link".to_string(),
            trimmed.to_string(),
            String::new(),
        );
    };

    let title = parsed.host_str().unwrap_or("External link").to_string();
    let mut description = parsed.path().trim_matches('/').to_string();
    if let Some(query) = parsed.query() {
        if description.is_empty() {
            description = format!("?{}", query);
        } else {
            description = format!("{}?{}", description, query);
        }
    }
    if description.is_empty() {
        description = "Open link".to_string();
    }
    if description.len() > 120 {
        description.truncate(117);
        description.push_str("...");
    }

    let link_image_url = if link_url_has_image_extension(&parsed) {
        trimmed.to_string()
    } else {
        String::new()
    };

    (title, description, link_image_url)
}

async fn save_post_image_file(
    user_id: i32,
    file_name: Option<&str>,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<SavedPostImage, String> {
    if bytes.is_empty() {
        return Err("Uploaded image is empty".to_string());
    }
    if bytes.len() > MAX_POST_IMAGE_BYTES {
        return Err("Each image must be 8MB or smaller".to_string());
    }

    detect_post_image_extension(file_name, content_type).ok_or_else(|| {
        "Only image files are allowed (.jpg, .png, .gif, .webp, .avif, .bmp)".to_string()
    })?;
    let compressed = compress_upload_to_jpeg(bytes.to_vec()).await?;
    let stem = sanitize_post_filename(file_name.unwrap_or("image"));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stored_name = format!(
        "{}_{}_{}.{}",
        timestamp, user_id, stem, COMPRESSED_IMAGE_EXTENSION
    );

    let mut dir = PathBuf::from("public/uploads/posts");
    dir.push(user_id.to_string());
    fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("Failed to prepare post upload folder: {}", err))?;

    let file_path = dir.join(&stored_name);
    fs::write(&file_path, compressed)
        .await
        .map_err(|_| "Failed to save uploaded post image".to_string())?;

    Ok(SavedPostImage {
        image_url: format!("/public/uploads/posts/{}/{}", user_id, stored_name),
        mime_type: COMPRESSED_IMAGE_MIME.to_string(),
    })
}


pub async fn load_post_visibility_state(pool: &PgPool, post_id: i64) -> (bool, bool) {
    sqlx::query_as::<_, (bool, bool)>(
        r#"
        SELECT
            EXISTS(
                SELECT 1
                FROM posts
                WHERE post_id = $1
            ) AS post_exists,
            EXISTS(
                SELECT 1
                FROM posts
                WHERE post_id = $1
                  AND BTRIM(COALESCE(body, '')) = $2
            ) AS is_soft_deleted
        "#,
    )
    .bind(post_id)
    .bind(MODERATOR_REDACTED_POST_TEXT)
    .fetch_one(pool)
    .await
    .unwrap_or((false, false))
}

pub async fn can_view_post_for_user(pool: &PgPool, post_id: i64, viewer_user_id: Option<i32>) -> bool {
    let viewer_user_id = viewer_user_id.unwrap_or(0);
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM posts p
            WHERE p.post_id = $1
              AND (
                  p.community_id IS NULL
                  OR p.user_id = $2
                  OR EXISTS (
                      SELECT 1
                      FROM community_page cp
                      WHERE cp.community_id = p.community_id
                        AND LOWER(COALESCE(cp.visibility, 'public')) <> 'private'
                  )
                  OR (
                      $2 > 0
                      AND EXISTS (
                          SELECT 1
                          FROM community_member cm
                          WHERE cm.community_id = p.community_id
                            AND cm.user_id = $2
                      )
                  )
              )
              AND can_view_post(
                  p.user_id,
                  COALESCE(NULLIF(p.visibility, ''), 'public'),
                  $2
              )
        )
        "#,
    )
    .bind(post_id)
    .bind(viewer_user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

pub fn normalize_post_visibility(raw: &str) -> &'static str {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        POST_VISIBILITY_PUBLIC => POST_VISIBILITY_PUBLIC,
        POST_VISIBILITY_FOLLOWING => POST_VISIBILITY_FOLLOWING,
        POST_VISIBILITY_FRIENDS => POST_VISIBILITY_FRIENDS,
        POST_VISIBILITY_PRIVATE => POST_VISIBILITY_PRIVATE,
        _ => POST_VISIBILITY_PUBLIC,
    }
}

pub fn post_visibility_label(visibility: &str) -> &'static str {
    match normalize_post_visibility(visibility) {
        POST_VISIBILITY_PUBLIC => "Public",
        POST_VISIBILITY_FOLLOWING => "Following",
        POST_VISIBILITY_FRIENDS => "Friends",
        POST_VISIBILITY_PRIVATE => "Private",
        _ => "Public",
    }
}



fn build_post_preview(
    body: &str,
    link_url: &str,
    first_image_url: &str,
    max_chars: usize,
) -> String {
    let body = body.trim();
    if !body.is_empty() {
        return truncate_preview(body, max_chars);
    }

    let link_url = link_url.trim();
    if !link_url.is_empty() {
        return truncate_preview(link_url, max_chars);
    }

    let first_image_url = first_image_url.trim();
    if !first_image_url.is_empty() {
        return truncate_preview(first_image_url, max_chars);
    }

    String::new()
}



/*  -----------------------------------------------------
    |                                                   |
    | Interactions section                              |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(sqlx::FromRow)]
struct PostInteractionRow {
    post_id: i64,
    likes_count: i64,
    dislikes_count: i64,
    comments_count: i64,
    shares_count: i64,
    liked_by_current_user: bool,
    disliked_by_current_user: bool,
    shared_by_current_user: bool,
}

async fn load_post_interactions_by_post(
    pool: &PgPool,
    post_ids: &[i64],
    current_user_id: Option<i32>,
) -> BTreeMap<i64, PostInteractionRow> {
    if post_ids.is_empty() {
        return BTreeMap::new();
    }

    let current_user_id = current_user_id.unwrap_or(0);
    let rows = sqlx::query_as::<_, PostInteractionRow>(
        r#"
        SELECT
            p.post_id,
            COALESCE(l.likes_count, 0)::BIGINT AS likes_count,
            COALESCE(d.dislikes_count, 0)::BIGINT AS dislikes_count,
            COALESCE(c.comments_count, 0)::BIGINT AS comments_count,
            COALESCE(s.shares_count, 0)::BIGINT AS shares_count,
            CASE
                WHEN $2 > 0 THEN EXISTS (
                    SELECT 1
                    FROM post_like pl
                    WHERE pl.post_id = p.post_id
                      AND pl.user_id = $2
                )
                ELSE FALSE
            END AS liked_by_current_user,
            CASE
                WHEN $2 > 0 THEN EXISTS (
                    SELECT 1
                    FROM post_dislike pd
                    WHERE pd.post_id = p.post_id
                      AND pd.user_id = $2
                )
                ELSE FALSE
            END AS disliked_by_current_user,
            CASE
                WHEN $2 > 0 THEN EXISTS (
                    SELECT 1
                    FROM post_share ps
                    WHERE ps.post_id = p.post_id
                      AND ps.user_id = $2
                )
                ELSE FALSE
            END AS shared_by_current_user
        FROM posts p
        LEFT JOIN (
            SELECT post_id, COUNT(*)::BIGINT AS likes_count
            FROM post_like
            WHERE post_id = ANY($1)
            GROUP BY post_id
        ) l ON l.post_id = p.post_id
        LEFT JOIN (
            SELECT post_id, COUNT(*)::BIGINT AS dislikes_count
            FROM post_dislike
            WHERE post_id = ANY($1)
            GROUP BY post_id
        ) d ON d.post_id = p.post_id
        LEFT JOIN (
            SELECT post_id, COUNT(*)::BIGINT AS comments_count
            FROM post_comment
            WHERE post_id = ANY($1)
            GROUP BY post_id
        ) c ON c.post_id = p.post_id
        LEFT JOIN (
            SELECT post_id, COUNT(*)::BIGINT AS shares_count
            FROM post_share
            WHERE post_id = ANY($1)
            GROUP BY post_id
        ) s ON s.post_id = p.post_id
        WHERE p.post_id = ANY($1)
        "#,
    )
    .bind(post_ids)
    .bind(current_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut by_post = BTreeMap::new();
    for row in rows {
        by_post.insert(row.post_id, row);
    }
    by_post
}



/*  -----------------------------------------------------
    |                                                   |
    | Comments section                                  |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(sqlx::FromRow)]
pub struct PostCommentRow {
    post_id: i64,
    comment_id: i64,
    commenter_public_id: i64,
    commenter_username: String,
    commenter_profile_photo_url: String,
    commenter_profile_photo_style: String,
    body: String,
    created_at: String,
    likes_count: i64,
    liked_by_current_user: bool,
    reply_parent_comment_id: i64,
    reply_to_comment_id: Option<i64>,
    reply_to_body_preview: String,
    reply_to_username: String,
}

#[derive(sqlx::FromRow)]
pub struct PostCommentOwnerRow {
    post_id: i64,
    user_id: i32,
}

#[derive(sqlx::FromRow)]
pub struct PostCommentPostRow {
    pub post_id: i64,
    pub comment_owner_user_id: i32,
}

#[derive(sqlx::FromRow)]
pub struct PostCommentPermissionRow {
    post_id: i64,
    comment_owner_user_id: i32,
    post_owner_user_id: i32,
}

async fn load_post_comments_by_post(
    pool: &PgPool,
    post_ids: &[i64],
    current_user_id: Option<i32>,
) -> BTreeMap<i64, Vec<PostCommentView>> {
    if post_ids.is_empty() {
        return BTreeMap::new();
    }

    let current_user_id = current_user_id.unwrap_or(0);
    let rows = sqlx::query_as::<_, PostCommentRow>(
        r#"
        SELECT
            c.post_id,
            c.comment_id,
            u.public_id AS commenter_public_id,
            u.username AS commenter_username,
            COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS commenter_profile_photo_url,
            COALESCE(u.profile_photo_style, '') AS commenter_profile_photo_style,
            COALESCE(NULLIF(c.body, ''), '') AS body,
            COALESCE(c.created_at::TEXT, '') AS created_at,
            COALESCE(cl.likes_count, 0)::BIGINT AS likes_count,
            CASE
                WHEN $2 > 0 THEN EXISTS (
                    SELECT 1
                    FROM post_comment_like pcl_self
                    WHERE pcl_self.comment_id = c.comment_id
                      AND pcl_self.user_id = $2
                )
                ELSE FALSE
            END AS liked_by_current_user,
            COALESCE(c.reply_to_comment_id, 0)::BIGINT AS reply_parent_comment_id,
            c.reply_to_comment_id,
            COALESCE(NULLIF(reply_comment.body, ''), '') AS reply_to_body_preview,
            COALESCE(NULLIF(reply_user.username, ''), '') AS reply_to_username
        FROM post_comment c
        JOIN users u ON u.id = c.user_id
        LEFT JOIN post_comment reply_comment ON reply_comment.comment_id = c.reply_to_comment_id
        LEFT JOIN users reply_user ON reply_user.id = reply_comment.user_id
        LEFT JOIN (
            SELECT
                pcl.comment_id,
                COUNT(*)::BIGINT AS likes_count
            FROM post_comment_like pcl
            JOIN post_comment c_like ON c_like.comment_id = pcl.comment_id
            WHERE c_like.post_id = ANY($1)
            GROUP BY pcl.comment_id
        ) cl ON cl.comment_id = c.comment_id
        WHERE c.post_id = ANY($1)
        ORDER BY c.created_at ASC, c.comment_id ASC
        "#,
    )
    .bind(post_ids)
    .bind(current_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut comments_by_post: BTreeMap<i64, Vec<PostCommentView>> = BTreeMap::new();
    for row in rows {
        comments_by_post
            .entry(row.post_id)
            .or_default()
            .push(PostCommentView {
                comment_id: row.comment_id,
                commenter_public_id: row.commenter_public_id,
                commenter_username: row.commenter_username,
                commenter_profile_photo_url: row.commenter_profile_photo_url,
                commenter_profile_photo_style: row.commenter_profile_photo_style,
                body: row.body,
                created_at: row.created_at,
                likes_count: row.likes_count,
                liked_by_current_user: row.liked_by_current_user,
                reply_parent_comment_id: row.reply_parent_comment_id,
                reply_to_comment_id: row.reply_to_comment_id,
                reply_to_body_preview: truncate_preview(row.reply_to_body_preview.trim(), 120),
                reply_to_username: row.reply_to_username,
                thread_depth: 0,
                thread_root_comment_id: row.comment_id,
            });
    }

    for comments in comments_by_post.values_mut() {
        *comments = thread_post_comments(comments.clone());
    }

    comments_by_post
}

fn thread_post_comments(comments: Vec<PostCommentView>) -> Vec<PostCommentView> {
    if comments.is_empty() {
        return comments;
    }

    let mut comments_by_id: BTreeMap<i64, PostCommentView> = BTreeMap::new();
    let mut ordered_ids: Vec<i64> = Vec::with_capacity(comments.len());
    for comment in comments {
        ordered_ids.push(comment.comment_id);
        comments_by_id.insert(comment.comment_id, comment);
    }

    let mut children_by_parent: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    let mut root_ids: Vec<i64> = Vec::new();
    for comment_id in &ordered_ids {
        let parent_id = comments_by_id
            .get(comment_id)
            .and_then(|comment| comment.reply_to_comment_id);
        if let Some(parent_id) = parent_id {
            if parent_id != *comment_id && comments_by_id.contains_key(&parent_id) {
                children_by_parent
                    .entry(parent_id)
                    .or_default()
                    .push(*comment_id);
                continue;
            }
        }
        root_ids.push(*comment_id);
    }

    fn walk_comment_thread(
        comment_id: i64,
        depth: i64,
        root_comment_id: i64,
        comments_by_id: &BTreeMap<i64, PostCommentView>,
        children_by_parent: &BTreeMap<i64, Vec<i64>>,
        visited: &mut BTreeSet<i64>,
        output: &mut Vec<PostCommentView>,
    ) {
        if visited.contains(&comment_id) {
            return;
        }

        let Some(comment) = comments_by_id.get(&comment_id) else {
            return;
        };

        visited.insert(comment_id);
        let mut threaded = comment.clone();
        threaded.thread_depth = depth;
        threaded.thread_root_comment_id = root_comment_id;
        output.push(threaded);

        if let Some(children) = children_by_parent.get(&comment_id) {
            for child_id in children {
                walk_comment_thread(
                    *child_id,
                    depth.saturating_add(1),
                    root_comment_id,
                    comments_by_id,
                    children_by_parent,
                    visited,
                    output,
                );
            }
        }
    }

    let mut visited: BTreeSet<i64> = BTreeSet::new();
    let mut threaded: Vec<PostCommentView> = Vec::with_capacity(ordered_ids.len());
    for root_id in root_ids {
        walk_comment_thread(
            root_id,
            0,
            root_id,
            &comments_by_id,
            &children_by_parent,
            &mut visited,
            &mut threaded,
        );
    }
    for comment_id in ordered_ids {
        if visited.contains(&comment_id) {
            continue;
        }
        walk_comment_thread(
            comment_id,
            0,
            comment_id,
            &comments_by_id,
            &children_by_parent,
            &mut visited,
            &mut threaded,
        );
    }

    threaded
}


async fn load_post_images_by_post(pool: &PgPool, post_ids: &[i64]) -> BTreeMap<i64, Vec<String>> {
    if post_ids.is_empty() {
        return BTreeMap::new();
    }

    let image_rows = sqlx::query_as::<_, FeedPostImageRow>(
        r#"
        SELECT post_id, image_url
        FROM post_image
        WHERE post_id = ANY($1)
        ORDER BY sort_order ASC, image_id ASC
        "#,
    )
    .bind(post_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut images_by_post: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for image_row in image_rows {
        images_by_post
            .entry(image_row.post_id)
            .or_default()
            .push(image_row.image_url);
    }
    images_by_post
}


#[derive(Deserialize)]
pub struct EditPostCommentForm {
    pub body: String,
}


pub async fn edit_post_comment(
    session: Session,
    State(pool): State<PgPool>,
    Path(comment_id): Path<i64>,
    headers: HeaderMap,
    Form(form): Form<EditPostCommentForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let owner = sqlx::query_as::<_, PostCommentOwnerRow>(
        r#"
        SELECT post_id, user_id
        FROM post_comment
        WHERE comment_id = $1
        "#,
    )
    .bind(comment_id)
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

    let (post_exists, is_soft_deleted) = load_post_visibility_state(&pool, owner.post_id).await;
    if !post_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if is_soft_deleted {
        if is_fetch_request(&headers) {
            return (
                StatusCode::CONFLICT,
                "This post was deleted by a moderator and comments can no longer be edited.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }
    if !can_view_post_for_user(&pool, owner.post_id, Some(current_user_id)).await {
        if is_fetch_request(&headers) {
            return (
                StatusCode::FORBIDDEN,
                "You cannot edit comments on this post.",
            )
                .into_response();
        }
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        UPDATE post_comment
        SET body = $1,
            updated_at = NOW()
        WHERE comment_id = $2
          AND user_id = $3
        "#,
    )
    .bind(&comment)
    .bind(comment_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("edit_post_comment update failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to update your comment right now",
        )
            .into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}


pub async fn delete_post_comment(
    session: Session,
    State(pool): State<PgPool>,
    Path(comment_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let permission = sqlx::query_as::<_, PostCommentPermissionRow>(
        r#"
        SELECT
            c.post_id,
            c.user_id AS comment_owner_user_id,
            p.user_id AS post_owner_user_id
        FROM post_comment c
        JOIN posts p ON p.post_id = c.post_id
        WHERE c.comment_id = $1
        "#,
    )
    .bind(comment_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    let Some(permission) = permission else {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    };

    if current_user_id != permission.comment_owner_user_id
        && current_user_id != permission.post_owner_user_id
    {
        return StatusCode::FORBIDDEN.into_response();
    }

    let should_notify_comment_owner = current_user_id == permission.post_owner_user_id
        && permission.comment_owner_user_id != permission.post_owner_user_id;

    let delete_result = sqlx::query(
        r#"
        DELETE FROM post_comment
        WHERE comment_id = $1
          AND post_id = $2
        "#,
    )
    .bind(comment_id)
    .bind(permission.post_id)
    .execute(&pool)
    .await;

    if let Err(err) = delete_result {
        tracing::warn!("delete_post_comment failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to delete comment right now",
        )
            .into_response();
    }

    if should_notify_comment_owner {
        let post_owner_username = load_user_identity(&pool, current_user_id)
            .await
            .map(|(username, _)| username)
            .unwrap_or_else(|| "post-owner".to_string());
        let body = format!(
            "@{} deleted your comment on their post",
            post_owner_username
        );
        let link_url = format!("/posts/{}", permission.post_id);
        if let Err(err) = create_notification(
            &pool,
            permission.comment_owner_user_id,
            "comment_deleted_by_post_owner",
            "Comment deleted",
            &body,
            &link_url,
        )
        .await
        {
            tracing::warn!(
                "delete_post_comment notification failed (comment_id={}): {}",
                comment_id,
                err
            );
        }
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}



/*  -----------------------------------------------------
    |                                                   |
    | Community Post section                            |
    |                                                   |
    -----------------------------------------------------
*/

pub async fn ensure_community_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS community_page (
            community_id BIGSERIAL PRIMARY KEY,
            slug TEXT NOT NULL DEFAULT '',
            name TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            profile_photo_url TEXT NOT NULL DEFAULT '/public/avatar.webp',
            profile_photo_style TEXT NOT NULL DEFAULT '',
            owner_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
            visibility TEXT NOT NULL DEFAULT 'public',
            status TEXT NOT NULL DEFAULT 'active',
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS slug TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS name TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS profile_photo_url TEXT NOT NULL DEFAULT '/public/avatar.webp'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS profile_photo_style TEXT NOT NULL DEFAULT ''
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS owner_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'public'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'active'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE community_page
        ADD COLUMN IF NOT EXISTS updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE community_page
        SET status = 'active'
        WHERE LOWER(COALESCE(status, '')) NOT IN ('active', 'hidden', 'deleted', 'banned')
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_page_status_idx
        ON community_page (status, community_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_page_slug_idx
        ON community_page (LOWER(slug))
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_page_name_idx
        ON community_page (LOWER(name))
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS community_member (
            community_id BIGINT NOT NULL REFERENCES community_page(community_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            joined_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (community_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_member_user_idx
        ON community_member (user_id, joined_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS community_rule (
            rule_id BIGSERIAL PRIMARY KEY,
            community_id BIGINT NOT NULL REFERENCES community_page(community_id) ON DELETE CASCADE,
            title TEXT NOT NULL DEFAULT '',
            body TEXT NOT NULL DEFAULT '',
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_rule_order_idx
        ON community_rule (community_id, sort_order ASC, rule_id ASC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS community_moderator (
            community_id BIGINT NOT NULL REFERENCES community_page(community_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            granted_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
            granted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (community_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_moderator_user_idx
        ON community_moderator (user_id, granted_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS community_ignore (
            community_id BIGINT NOT NULL REFERENCES community_page(community_id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            ignored_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (community_id, user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS community_ignore_user_idx
        ON community_ignore (user_id, ignored_at DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE posts
        ADD COLUMN IF NOT EXISTS community_id BIGINT REFERENCES community_page(community_id) ON DELETE SET NULL
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS posts_community_created_idx
        ON posts (community_id, created_at DESC, post_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO community_member (community_id, user_id, joined_at)
        SELECT
            c.community_id,
            c.owner_user_id,
            COALESCE(c.created_at, CURRENT_TIMESTAMP)
        FROM community_page c
        WHERE c.owner_user_id IS NOT NULL
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO community_moderator (community_id, user_id, granted_by_user_id, granted_at)
        SELECT
            c.community_id,
            c.owner_user_id,
            c.owner_user_id,
            COALESCE(c.created_at, CURRENT_TIMESTAMP)
        FROM community_page c
        WHERE c.owner_user_id IS NOT NULL
        ON CONFLICT (community_id, user_id) DO NOTHING
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}


pub async fn load_community_posts(
    pool: &PgPool,
    community_id: i64,
    current_user_id: Option<i32>,
    sort: &str,
) -> Vec<IndexPostView> {
    if community_id <= 0 {
        return Vec::new();
    }

    let viewer_user_id = current_user_id.unwrap_or(0);
    let safe_sort = normalize_community_sort(Some(sort));
    let order_by_clause = match safe_sort {
        "new" => "p.created_at DESC, p.post_id DESC",
        "top" => {
            "(COALESCE((SELECT COUNT(*)::BIGINT FROM post_like pl WHERE pl.post_id = p.post_id), 0) - \
              COALESCE((SELECT COUNT(*)::BIGINT FROM post_dislike pd WHERE pd.post_id = p.post_id), 0)) DESC, \
             COALESCE((SELECT COUNT(*)::BIGINT FROM post_comment pc WHERE pc.post_id = p.post_id), 0) DESC, \
             p.created_at DESC, p.post_id DESC"
        }
        _ => {
            "((COALESCE((SELECT COUNT(*)::BIGINT FROM post_like pl WHERE pl.post_id = p.post_id), 0) - \
               COALESCE((SELECT COUNT(*)::BIGINT FROM post_dislike pd WHERE pd.post_id = p.post_id), 0)) * 4 + \
              COALESCE((SELECT COUNT(*)::BIGINT FROM post_comment pc WHERE pc.post_id = p.post_id), 0) * 3 + \
              CASE \
                  WHEN p.created_at >= NOW() - INTERVAL '24 hours' THEN 10 \
                  WHEN p.created_at >= NOW() - INTERVAL '3 days' THEN 6 \
                  WHEN p.created_at >= NOW() - INTERVAL '7 days' THEN 3 \
                  ELSE 0 \
              END) DESC, \
             p.created_at DESC, p.post_id DESC"
        }
    };
    let query = format!(
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
        WHERE p.community_id = $1
          AND (
              p.user_id = $2
              OR LOWER(COALESCE(c.visibility, 'public')) <> 'private'
              OR EXISTS (
                  SELECT 1
                  FROM community_member cm
                  WHERE cm.community_id = p.community_id
                    AND cm.user_id = $2
              )
          )
          AND can_view_post(
              p.user_id,
              COALESCE(NULLIF(p.visibility, ''), 'public'),
              $2
          )
        ORDER BY {}
        LIMIT $3
        "#,
        order_by_clause
    );
    let rows = sqlx::query_as::<_, FeedPostRow>(&query)
        .bind(community_id)
        .bind(viewer_user_id)
        .bind(FEED_PAGE_MAX_LIMIT)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    build_feed_post_views(pool, rows, current_user_id).await
}



/*  -----------------------------------------------------
    |                                                   |
    | Moderator Post section                            |
    |                                                   |
    -----------------------------------------------------
*/

pub async fn ensure_moderation_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS app_report (
            report_id BIGSERIAL PRIMARY KEY,
            reporter_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            kind TEXT NOT NULL DEFAULT 'post',
            target_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
            target_post_id BIGINT REFERENCES posts(post_id) ON DELETE SET NULL,
            target_message_id INTEGER REFERENCES messages(msg_id) ON DELETE SET NULL,
            reason TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            modified_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_report_status_created_idx
        ON app_report (status, created_at DESC, report_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE app_report
        ADD COLUMN IF NOT EXISTS target_message_id INTEGER REFERENCES messages(msg_id) ON DELETE SET NULL
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_report_target_post_idx
        ON app_report (target_post_id)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_report_target_user_idx
        ON app_report (target_user_id)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_report_target_message_idx
        ON app_report (target_message_id)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS moderator_post_status (
            post_id BIGINT PRIMARY KEY REFERENCES posts(post_id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'open',
            verified_at TIMESTAMP NULL,
            modified_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS moderator_post_status_status_idx
        ON moderator_post_status (status, modified_at DESC, post_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}