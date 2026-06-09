use axum::{extract::{Path, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Redirect}};
use serde::{Serialize, Deserialize};
use sqlx::FromRow;

pub use sqlx::PgPool;
use tower_sessions::Session;

use crate::{CANONICAL_INSTAVOX_DOMAIN, handlers::{searching::is_fetch_request, session::session_user_id}};

pub const NOTIFICATION_PAGE_DEFAULT_LIMIT: i64 = 20;
pub const NOTIFICATION_PAGE_MAX_LIMIT: i64 = 60;


/*  -----------------------------------------------------
    |                                                   |
    | Notification section                                  |
    |                                                   |
    -----------------------------------------------------
*/

#[derive(sqlx::FromRow)]
pub struct HeaderNotificationRow {
    pub notification_id: i64,
    pub title: String,
    pub body: String,
    pub link_url: String,
    pub is_read: bool,
    pub created_at: String,
    pub message_count: i64,
}


#[derive(Deserialize)]
pub struct NotificationPageQuery {
    pub before_notification_id: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct NotificationPageResponse {
    pub notifications: Vec<NotificationPageItem>,
    pub has_more: bool,
    pub next_before_notification_id: Option<i64>,
}

#[derive(Serialize)]
pub struct NotificationPageItem {
    pub notification_id: i64,
    pub title: String,
    pub body: String,
    pub link_url: String,
    pub created_at: String,
    pub is_unread: bool,
    pub message_count: i64,
}


pub async fn create_notification(
    pool: &PgPool,
    recipient_user_id: i32,
    kind: &str,
    title: &str,
    body: &str,
    link_url: &str,
) -> Result<(), sqlx::Error> {
    let (notification_id, created_at) = sqlx::query_as::<_, (i64, String)>(
        r#"
        INSERT INTO app_notification (user_id, kind, title, body, link_url, is_read, created_at, message_count)
        VALUES ($1, $2, $3, $4, $5, 'false', NOW()::TEXT, 1)
        RETURNING notification_id, COALESCE(created_at::TEXT, NOW()::TEXT)
        "#,
    )
    .bind(recipient_user_id)
    .bind(kind)
    .bind(title)
    .bind(body)
    .bind(link_url)
    .fetch_one(pool)
    .await?;

    let unread_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::BIGINT
        FROM app_notification
        WHERE user_id = $1
          AND LOWER(COALESCE(is_read::TEXT, 'false')) NOT IN ('t', 'true', '1', 'yes', 'y')
        "#,
    )
    .bind(recipient_user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    push_header_notification_event(
        recipient_user_id,
        notification_id,
        kind,
        title,
        body,
        link_url,
        &created_at,
        unread_count,
        1,
    )
    .await;

    Ok(())
}

pub async fn create_comment_like_notification(
    pool: &PgPool,
    recipient_user_id: i32,
    body: &str,
    link_url: &str,
) -> Result<(), sqlx::Error> {
    let title = "New comment like";
    let updated = sqlx::query_as::<_, (i64, String)>(
        r#"
        UPDATE app_notification
        SET
            title = $2,
            body = $3,
            is_read = 'false',
            created_at = NOW()::TEXT,
            message_count = 1
        WHERE notification_id = (
            SELECT notification_id
            FROM app_notification
            WHERE user_id = $1
              AND kind = 'comment_like'
              AND link_url = $4
              AND LOWER(COALESCE(is_read::TEXT, 'false')) NOT IN ('t', 'true', '1', 'yes', 'y')
            ORDER BY notification_id DESC
            LIMIT 1
        )
        RETURNING notification_id, COALESCE(created_at::TEXT, NOW()::TEXT)
        "#,
    )
    .bind(recipient_user_id)
    .bind(title)
    .bind(body)
    .bind(link_url)
    .fetch_optional(pool)
    .await?;

    let (notification_id, created_at) = match updated {
        Some(row) => row,
        None => {
            sqlx::query_as::<_, (i64, String)>(
                r#"
                INSERT INTO app_notification (user_id, kind, title, body, link_url, is_read, created_at, message_count)
                VALUES ($1, 'comment_like', $2, $3, $4, 'false', NOW()::TEXT, 1)
                RETURNING notification_id, COALESCE(created_at::TEXT, NOW()::TEXT)
                "#,
            )
            .bind(recipient_user_id)
            .bind(title)
            .bind(body)
            .bind(link_url)
            .fetch_one(pool)
            .await?
        }
    };

    let unread_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::BIGINT
        FROM app_notification
        WHERE user_id = $1
          AND LOWER(COALESCE(is_read::TEXT, 'false')) NOT IN ('t', 'true', '1', 'yes', 'y')
        "#,
    )
    .bind(recipient_user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    push_header_notification_event(
        recipient_user_id,
        notification_id,
        "comment_like",
        title,
        body,
        link_url,
        &created_at,
        unread_count,
        1,
    )
    .await;

    Ok(())
}


#[derive(Serialize)]
pub struct HeaderNotificationView {
    pub notification_id: i64,
    pub title: String,
    pub body: String,
    pub link_url: String,
    pub created_at: String,
    pub is_unread: bool,
    pub message_count: i64,
}


pub async fn ensure_notification_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS app_notification (
            notification_id BIGSERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            kind TEXT NOT NULL DEFAULT 'system',
            title TEXT NOT NULL DEFAULT 'Notification',
            body TEXT NOT NULL DEFAULT '',
            link_url TEXT NOT NULL DEFAULT '/',
            is_read TEXT NOT NULL DEFAULT 'false',
            created_at TEXT NOT NULL DEFAULT NOW()::TEXT,
            message_count BIGINT NOT NULL DEFAULT 1
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        ALTER TABLE app_notification
        ADD COLUMN IF NOT EXISTS message_count BIGINT NOT NULL DEFAULT 1
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_notification_user_idx
        ON app_notification (user_id, notification_id DESC)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS app_notification_unread_idx
        ON app_notification (user_id, is_read)
        "#,
    )
    .execute(pool)
    .await?;

    // Migrate legacy message links like /messenger?with=<public_id> to
    // /messenger?user=<username>@instavox.social
    sqlx::query(
        r#"
        WITH legacy AS (
            SELECT
                n.notification_id,
                SUBSTRING(n.link_url FROM '/messenger\?with=([0-9]+)') AS with_id
            FROM app_notification n
            WHERE n.kind = 'message'
              AND n.link_url LIKE '/messenger?with=%'
        )
        UPDATE app_notification n
        SET link_url = '/messenger?user=' || COALESCE(NULLIF(u.username, ''), 'user') || '@' || $1
        FROM legacy l
        JOIN users u
          ON (
                u.public_id::TEXT = l.with_id
             OR u.id::TEXT = l.with_id
          )
        WHERE n.notification_id = l.notification_id
        "#,
    )
    .bind(CANONICAL_INSTAVOX_DOMAIN)
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        DO $$
        BEGIN
            IF to_regclass('public.notification') IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM app_notification) THEN
                BEGIN
                    INSERT INTO app_notification (user_id, kind, title, body, link_url, is_read, created_at)
                    SELECT
                        oldn.user_id::INTEGER,
                        COALESCE(NULLIF(oldn.kind::TEXT, ''), 'system'),
                        COALESCE(NULLIF(oldn.title::TEXT, ''), 'Notification'),
                        COALESCE(oldn.body::TEXT, ''),
                        COALESCE(NULLIF(oldn.link_url::TEXT, ''), '/'),
                        COALESCE(oldn.is_read::TEXT, 'false'),
                        COALESCE(oldn.created_at::TEXT, NOW()::TEXT)
                    FROM notification oldn
                    WHERE oldn.user_id IS NOT NULL;
                EXCEPTION
                    WHEN undefined_column THEN
                        NULL;
                    WHEN datatype_mismatch THEN
                        NULL;
                    WHEN invalid_text_representation THEN
                        NULL;
                    WHEN undefined_table THEN
                        NULL;
                END;
            END IF;
        END
        $$;
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}


pub async fn load_header_notifications(
    pool: &PgPool,
    current_user_id: Option<i32>,
) -> (i64, Vec<HeaderNotificationView>) {
    let Some(current_user_id) = current_user_id else {
        return (0, Vec::new());
    };

    let unread_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)::BIGINT
        FROM app_notification
        WHERE user_id = $1
          AND LOWER(COALESCE(is_read::TEXT, 'false')) NOT IN ('t', 'true', '1', 'yes', 'y')
        "#,
    )
    .bind(current_user_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|err| {
        tracing::warn!("load_header_notifications unread query failed: {}", err);
        0
    });

    let rows = sqlx::query_as::<_, HeaderNotificationRow>(
        r#" 
        SELECT 
            notification_id, 
            COALESCE(NULLIF(title, ''), 
            'Notification') AS title, 
            COALESCE(NULLIF(body, ''), '') AS body, 
            COALESCE(NULLIF(link_url, ''), '/') AS link_url, 
        CASE WHEN LOWER(
            COALESCE(is_read::TEXT, 'false')) 
        IN ('t', 'true', '1', 'yes', 'y') 
        THEN TRUE ELSE FALSE END AS is_read, 
            COALESCE(created_at::TEXT, '') AS created_at, 
            COALESCE(message_count, 1)::BIGINT AS message_count
        FROM app_notification 
        WHERE user_id = $1 ORDER BY notification_id DESC LIMIT 12"#,
    )
    .bind(current_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|err| {
        tracing::warn!("load_header_notifications list query failed: {}", err);
        Vec::new()
    });

    let notifications = rows
        .into_iter()
        .map(|row| HeaderNotificationView {
            notification_id: row.notification_id,
            title: row.title,
            body: row.body,
            link_url: row.link_url,
            created_at: row.created_at,
            is_unread: !row.is_read,
            message_count: row.message_count.max(1),
        })
        .collect();

    (unread_count, notifications)
}


pub async fn mark_notifications_read_all(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE app_notification
        SET is_read = 'true'
        WHERE user_id = $1
          AND LOWER(COALESCE(is_read::TEXT, 'false')) NOT IN ('t', 'true', '1', 'yes', 'y')
        "#,
    )
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("mark_notifications_read_all failed: {}", err);
    }

    if is_fetch_request(&headers) {
        return StatusCode::OK.into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn mark_notification_read(
    session: Session,
    State(pool): State<PgPool>,
    Path(notification_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE app_notification
        SET is_read = 'true'
        WHERE notification_id = $1
          AND user_id = $2
        "#,
    )
    .bind(notification_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("mark_notification_read failed: {}", err);
    }

    if is_fetch_request(&headers) {
        return StatusCode::OK.into_response();
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}


pub async fn delete_notification(
    session: Session,
    State(pool): State<PgPool>,
    Path(notification_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM app_notification
        WHERE notification_id = $1
          AND user_id = $2
        "#,
    )
    .bind(notification_id)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("delete_notification failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn delete_notifications_all(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM app_notification
        WHERE user_id = $1
        "#,
    )
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("delete_notifications_all failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}