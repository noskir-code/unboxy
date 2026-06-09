#[derive(Template)]
#[template(path = "messenger.html")]
#[allow(dead_code)]
pub struct MessengerTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub selected_user_public_id: i64,
    pub selected_group_id: i32,
    pub threads: Vec<MessengerThread>,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
}

#[derive(Deserialize)]
pub struct MessengerQuery {
    pub user: Option<String>,
    pub with: Option<i64>,
    pub group: Option<i32>,
}

#[derive(sqlx::FromRow)]
pub struct MessengerConversation {
    pub user_id: i32,
    pub user_public_id: i64,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub last_message: String,
    pub last_sender_id: Option<i32>,
    pub last_attachment_count: i64,
    pub last_time: String,
}

#[derive(sqlx::FromRow)]
pub struct MessengerGroupConversation {
    pub chat_id: i32,
    pub group_name: String,
    pub last_message: String,
    pub last_sender_id: Option<i32>,
    pub last_attachment_count: i64,
    pub last_time: String,
}

#[derive(Serialize)]
pub struct MessengerThread {
    pub is_group: bool,
    pub user_id: i32,
    pub user_public_id: i64,
    pub user_lookup: String,
    pub chat_id: i32,
    pub name: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub last_message: String,
    pub last_time: String,
}

fn message_contains_http_link(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let candidate = token.trim_matches(|ch: char| {
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
        });
        let lower = candidate.to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    })
}

fn format_last_thread_message(
    current_user_id: i32,
    raw_message: &str,
    last_sender_id: Option<i32>,
    last_attachment_count: i64,
) -> String {
    let text = raw_message.trim();
    let sent_by_current_user = last_sender_id == Some(current_user_id);

    if message_contains_http_link(text) {
        if sent_by_current_user {
            return "You sent a link".to_string();
        }
        return "Sent you a link".to_string();
    }

    if last_attachment_count > 0 && text.is_empty() {
        let noun = if last_attachment_count > 1 {
            "attachments"
        } else {
            "attachment"
        };
        if sent_by_current_user {
            return format!("You sent {}", noun);
        }
        return format!("Sent you {}", noun);
    }

    text.to_string()
}

async fn load_messenger_threads(pool: &PgPool, current_user_id: i32) -> Vec<MessengerThread> {
    let conversations = sqlx::query_as::<_, MessengerConversation>(
            r#"
            SELECT
                rel.counterpart_id AS user_id,
                u.public_id AS user_public_id,
                u.username,
                COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                COALESCE(u.profile_photo_style, '') AS profile_photo_style,
                COALESCE(lm.msg, '') AS last_message,
                lm.sender AS last_sender_id,
                COALESCE(lm.attachment_count, 0) AS last_attachment_count,
                COALESCE(lm.created_at::TEXT, rel.modified_at::TEXT, rel.created_at::TEXT, '') AS last_time
            FROM (
                SELECT DISTINCT ON (counterpart_id)
                    counterpart_id,
                    friendship_id,
                    relationship_status,
                    modified_at,
                    created_at
                FROM (
                    SELECT
                        CASE
                            WHEN r.sender_id = $1 THEN r.receiver_id
                            ELSE r.sender_id
                        END AS counterpart_id,
                        r.friendship_id,
                        LOWER(COALESCE(r.status, '')) AS relationship_status,
                        r.modified_at,
                        r.created_at
                    FROM relationship r
                    WHERE r.sender_id = $1
                       OR r.receiver_id = $1
                ) relationship_history
                ORDER BY counterpart_id, friendship_id DESC
            ) rel
            JOIN users u ON u.id = rel.counterpart_id
            LEFT JOIN LATERAL (
                SELECT c.chat_id
                FROM chat c
                JOIN relationship r_chat
                  ON c.chat_type = 'friendship'
                 AND c.chat_title = r_chat.friendship_id
                WHERE (
                    r_chat.sender_id = $1
                    AND r_chat.receiver_id = rel.counterpart_id
                )
                   OR (
                    r_chat.sender_id = rel.counterpart_id
                    AND r_chat.receiver_id = $1
                )
                ORDER BY c.chat_id DESC
                LIMIT 1
            ) direct_chat ON TRUE
            LEFT JOIN LATERAL (
                SELECT
                    m.msg,
                    m.sender,
                    m.created_at,
                    m.msg_id,
                    COALESCE((
                        SELECT COUNT(*)::BIGINT
                        FROM message_attachment ma
                        WHERE ma.msg_id = m.msg_id
                    ), 0) AS attachment_count
                FROM messages m
                WHERE direct_chat.chat_id IS NOT NULL
                  AND m.chat_id = direct_chat.chat_id
                ORDER BY COALESCE(m.created_at, '1970-01-01'::timestamp) DESC, m.msg_id DESC
                LIMIT 1
            ) lm ON TRUE
            WHERE rel.relationship_status NOT IN ('blocked', 'block')
              AND (
                rel.relationship_status IN ('friend', 'friends', 'friendship', 'accepted', 'removed', 'remove')
                OR direct_chat.chat_id IS NOT NULL
              )
            ORDER BY COALESCE(lm.created_at, rel.modified_at, rel.created_at) DESC, rel.friendship_id DESC
            "#,
        )
    .bind(current_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let groups = sqlx::query_as::<_, MessengerGroupConversation>(
            r#"
            SELECT
                g.chat_id,
                COALESCE(NULLIF(g.group_name, ''), members.member_names) AS group_name,
                COALESCE(lm.msg, '') AS last_message,
                lm.sender AS last_sender_id,
                COALESCE(lm.attachment_count, 0) AS last_attachment_count,
                COALESCE(lm.created_at::TEXT, g.modified_at::TEXT, g.created_at::TEXT, '') AS last_time
            FROM chat_group g
            JOIN chat_member cm
              ON cm.chat_id = g.chat_id
             AND cm.user_id = $1
            LEFT JOIN LATERAL (
                SELECT
                    STRING_AGG(u2.username, ', ' ORDER BY cm2.joined_at ASC) AS member_names
                FROM chat_member cm2
                JOIN users u2
                  ON u2.id = cm2.user_id
                WHERE cm2.chat_id = g.chat_id
            ) members ON TRUE
            LEFT JOIN LATERAL (
                SELECT
                    m.msg,
                    m.sender,
                    m.created_at,
                    m.msg_id,
                    COALESCE((
                        SELECT COUNT(*)::BIGINT
                        FROM message_attachment ma
                        WHERE ma.msg_id = m.msg_id
                    ), 0) AS attachment_count
                FROM messages m
                WHERE m.chat_id = g.chat_id
                ORDER BY COALESCE(m.created_at, '1970-01-01'::timestamp) DESC, m.msg_id DESC
                LIMIT 1
            ) lm ON TRUE
            ORDER BY COALESCE(lm.created_at, g.modified_at, g.created_at) DESC, g.chat_id DESC
            "#,
        )
    .bind(current_user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let local_domain = local_profile_domain();
    let mut threads = Vec::with_capacity(conversations.len() + groups.len());
    for conversation in conversations {
        let username = conversation.username;
        let user_lookup = format!("{}@{}", username.trim(), local_domain);
        threads.push(MessengerThread {
            is_group: false,
            user_id: conversation.user_id,
            user_public_id: conversation.user_public_id,
            user_lookup,
            chat_id: 0,
            name: username,
            profile_photo_url: conversation.profile_photo_url,
            profile_photo_style: conversation.profile_photo_style,
            last_message: format_last_thread_message(
                current_user_id,
                &conversation.last_message,
                conversation.last_sender_id,
                conversation.last_attachment_count,
            ),
            last_time: conversation.last_time,
        });
    }

    for group in groups {
        threads.push(MessengerThread {
            is_group: true,
            user_id: 0,
            user_public_id: 0,
            user_lookup: String::new(),
            chat_id: group.chat_id,
            name: group.group_name,
            profile_photo_url: "/public/group.webp".to_string(),
            profile_photo_style: String::new(),
            last_message: format_last_thread_message(
                current_user_id,
                &group.last_message,
                group.last_sender_id,
                group.last_attachment_count,
            ),
            last_time: group.last_time,
        });
    }

    threads.sort_by(|a, b| {
        a.last_time
            .is_empty()
            .cmp(&b.last_time.is_empty())
            .then_with(|| b.last_time.cmp(&a.last_time))
    });
    threads
}

pub async fn messenger_threads(session: Session, State(pool): State<PgPool>) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    Json(load_messenger_threads(&pool, current_user_id).await).into_response()
}

pub async fn messenger(
    Query(query): Query<MessengerQuery>,
    session: Session,
    State(pool): State<PgPool>,
) -> impl IntoResponse {
    let current_user_id = session_user_id(&session).await;
    let current_public_id = session_public_user_id(&session).await.unwrap_or(0);
    let selected_user_public_id = if let Some(raw_user_lookup) = query.user.as_deref() {
        let lookup = raw_user_lookup.trim();
        let username_lookup = if let Some((username_part, domain_part)) = lookup.split_once('@') {
            let username_part = username_part.trim();
            let domain_part = domain_part.trim();
            if username_part.is_empty()
                || domain_part.is_empty()
                || !local_profile_domain_matches(domain_part)
            {
                None
            } else {
                Some(username_part.to_string())
            }
        } else if lookup.is_empty() {
            None
        } else {
            Some(lookup.to_string())
        };

        if let Some(username_lookup) = username_lookup {
            sqlx::query_scalar::<_, i64>(
                r#"
                SELECT public_id
                FROM users
                WHERE LOWER(username) = LOWER($1)
                LIMIT 1
                "#,
            )
            .bind(username_lookup)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .or(query.with)
            .unwrap_or(0)
        } else {
            query.with.unwrap_or(0)
        }
    } else {
        query.with.unwrap_or(0)
    };
    let is_moderator = load_is_moderator(&pool, current_user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, current_user_id).await;
    let threads = if let Some(current_user_id) = current_user_id {
        load_messenger_threads(&pool, current_user_id).await
    } else {
        Vec::new()
    };

    let template = MessengerTemplate {
        title: "Messenger".to_string(),
        id: current_public_id,
        user_id: current_user_id.unwrap_or(0),
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        selected_user_public_id,
        selected_group_id: query.group.unwrap_or(0),
        threads,
        unread_notifications_count,
        notifications,
    };
    render_template_response(&template)
}

#[derive(Deserialize)]
pub struct CreateGroupPayload {
    pub group_name: Option<String>,
    pub member_ids: Vec<i32>,
}

#[derive(Serialize)]
pub struct CreateGroupResponse {
    pub chat_id: i32,
    pub url: String,
}

#[derive(Deserialize)]
pub struct AddGroupMemberPayload {
    pub member_id: i32,
}

pub async fn create_group(
    session: Session,
    State(pool): State<PgPool>,
    Json(payload): Json<CreateGroupPayload>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return (StatusCode::UNAUTHORIZED, "Login required").into_response();
    };

    let group_name = payload
        .group_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);

    let mut unique_members = BTreeSet::new();
    for member_id in payload.member_ids {
        if member_id != current_user_id {
            unique_members.insert(member_id);
        }
    }
    let member_ids: Vec<i32> = unique_members.into_iter().collect();

    for member_id in &member_ids {
        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM users
                WHERE id = $1
            )
            "#,
        )
        .bind(member_id)
        .fetch_one(&pool)
        .await
        .unwrap_or(false);

        if !exists {
            return (
                StatusCode::BAD_REQUEST,
                format!("User {} does not exist", member_id),
            )
                .into_response();
        }

        let is_friend = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM relationship
                WHERE (
                    (sender_id = $1 AND receiver_id = $2)
                    OR
                    (sender_id = $2 AND receiver_id = $1)
                )
                  AND LOWER(COALESCE(status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
            )
            "#,
        )
        .bind(current_user_id)
        .bind(member_id)
        .fetch_one(&pool)
        .await
        .unwrap_or(false);

        if !is_friend {
            return (
                StatusCode::BAD_REQUEST,
                format!("User {} is not your friend", member_id),
            )
                .into_response();
        }
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::warn!("create_group begin failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
        }
    };

    let next_chat_id =
        match sqlx::query_scalar::<_, i32>("SELECT COALESCE(MAX(chat_id), 0) + 1 FROM chat")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!("create_group next_chat_id failed: {}", err);
                let _ = tx.rollback().await;
                return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
            }
        };

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO chat (chat_id, chat_title, chat_type)
        VALUES ($1, NULL, 'group')
        "#,
    )
    .bind(next_chat_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("create_group insert chat failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO chat_group (chat_id, group_name, created_by, created_at, modified_at)
        VALUES ($1, $2, $3, NOW(), NOW())
        "#,
    )
    .bind(next_chat_id)
    .bind(group_name)
    .bind(current_user_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("create_group insert chat_group failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        INSERT INTO chat_member (chat_id, user_id, role, joined_at)
        VALUES ($1, $2, 'owner', NOW())
        "#,
    )
    .bind(next_chat_id)
    .bind(current_user_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("create_group insert owner failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
    }

    for member_id in member_ids {
        if let Err(err) = sqlx::query(
            r#"
            INSERT INTO chat_member (chat_id, user_id, role, joined_at)
            VALUES ($1, $2, 'member', NOW())
            ON CONFLICT (chat_id, user_id) DO NOTHING
            "#,
        )
        .bind(next_chat_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await
        {
            tracing::warn!("create_group insert member {} failed: {}", member_id, err);
            let _ = tx.rollback().await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::warn!("create_group commit failed: {}", err);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot create group").into_response();
    }

    (
        StatusCode::OK,
        Json(CreateGroupResponse {
            chat_id: next_chat_id,
            url: format!("/messenger?group={}", next_chat_id),
        }),
    )
        .into_response()
}

pub async fn add_group_member(
    session: Session,
    State(pool): State<PgPool>,
    Path(chat_id): Path<i32>,
    Json(payload): Json<AddGroupMemberPayload>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return (StatusCode::UNAUTHORIZED, "Login required").into_response();
    };

    let member_id = payload.member_id;
    if member_id <= 0 {
        return (StatusCode::BAD_REQUEST, "Invalid member id").into_response();
    }
    if member_id == current_user_id {
        return (StatusCode::BAD_REQUEST, "You are already in the group").into_response();
    }

    let can_access_group = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM chat_member
            WHERE chat_id = $1
              AND user_id = $2
        )
        "#,
    )
    .bind(chat_id)
    .bind(current_user_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !can_access_group {
        return (StatusCode::FORBIDDEN, "You are not a member of this group").into_response();
    }

    let is_group_chat = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM chat_group
            WHERE chat_id = $1
        )
        "#,
    )
    .bind(chat_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !is_group_chat {
        return (StatusCode::NOT_FOUND, "Group not found").into_response();
    }

    let user_exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE id = $1
        )
        "#,
    )
    .bind(member_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !user_exists {
        return (StatusCode::BAD_REQUEST, "User does not exist").into_response();
    }

    let is_friend = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM relationship
            WHERE (
                (sender_id = $1 AND receiver_id = $2)
                OR
                (sender_id = $2 AND receiver_id = $1)
            )
              AND LOWER(COALESCE(status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
        )
        "#,
    )
    .bind(current_user_id)
    .bind(member_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !is_friend {
        return (StatusCode::BAD_REQUEST, "User is not your friend").into_response();
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::warn!("add_group_member begin failed: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot add user to group",
            )
                .into_response();
        }
    };

    let insert_result = match sqlx::query(
        r#"
        INSERT INTO chat_member (chat_id, user_id, role, joined_at)
        VALUES ($1, $2, 'member', NOW())
        ON CONFLICT (chat_id, user_id) DO NOTHING
        "#,
    )
    .bind(chat_id)
    .bind(member_id)
    .execute(&mut *tx)
    .await
    {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!("add_group_member insert failed: {}", err);
            let _ = tx.rollback().await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot add user to group",
            )
                .into_response();
        }
    };

    if insert_result.rows_affected() > 0
        && let Err(err) = sqlx::query(
            r#"
            UPDATE chat_group
            SET modified_at = NOW()
            WHERE chat_id = $1
            "#,
        )
        .bind(chat_id)
        .execute(&mut *tx)
        .await
    {
        tracing::warn!("add_group_member touch group failed: {}", err);
        let _ = tx.rollback().await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot add user to group",
        )
            .into_response();
    }

    if let Err(err) = tx.commit().await {
        tracing::warn!("add_group_member commit failed: {}", err);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot add user to group",
        )
            .into_response();
    }

    if insert_result.rows_affected() > 0 {
        (StatusCode::OK, "User added to group").into_response()
    } else {
        (StatusCode::OK, "User is already in this group").into_response()
    }
}

pub async fn delete_group(
    session: Session,
    State(pool): State<PgPool>,
    Path(chat_id): Path<i32>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return (StatusCode::UNAUTHORIZED, "Login required").into_response();
    };

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::warn!("delete_group begin failed: {}", err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
        }
    };

    let created_by = match sqlx::query_scalar::<_, i32>(
        r#"
        SELECT created_by
        FROM chat_group
        WHERE chat_id = $1
        "#,
    )
    .bind(chat_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("delete_group query creator failed: {}", err);
            let _ = tx.rollback().await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
        }
    };

    let Some(created_by) = created_by else {
        let _ = tx.rollback().await;
        return (StatusCode::NOT_FOUND, "Group not found").into_response();
    };

    if created_by != current_user_id {
        let _ = tx.rollback().await;
        return (
            StatusCode::FORBIDDEN,
            "Only the group creator can delete this group",
        )
            .into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM message_attachment
        WHERE msg_id IN (
            SELECT msg_id
            FROM messages
            WHERE chat_id = $1
        )
        "#,
    )
    .bind(chat_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("delete_group delete attachments failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM messages
        WHERE chat_id = $1
        "#,
    )
    .bind(chat_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("delete_group delete messages failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM chat_member
        WHERE chat_id = $1
        "#,
    )
    .bind(chat_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("delete_group delete members failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        DELETE FROM chat_group
        WHERE chat_id = $1
        "#,
    )
    .bind(chat_id)
    .execute(&mut *tx)
    .await
    {
        tracing::warn!("delete_group delete group failed: {}", err);
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
    }

    let deleted_chat = match sqlx::query(
        r#"
        DELETE FROM chat
        WHERE chat_id = $1
          AND chat_type = 'group'
        "#,
    )
    .bind(chat_id)
    .execute(&mut *tx)
    .await
    {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!("delete_group delete chat failed: {}", err);
            let _ = tx.rollback().await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
        }
    };

    if deleted_chat.rows_affected() == 0 {
        let _ = tx.rollback().await;
        return (StatusCode::NOT_FOUND, "Group not found").into_response();
    }

    if let Err(err) = tx.commit().await {
        tracing::warn!("delete_group commit failed: {}", err);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Cannot delete group").into_response();
    }

    (StatusCode::OK, "Group deleted").into_response()
}

pub async fn group_members(
    session: Session,
    State(pool): State<PgPool>,
    Path(chat_id): Path<i32>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let is_group_chat = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM chat_group
            WHERE chat_id = $1
        )
        "#,
    )
    .bind(chat_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !is_group_chat {
        return StatusCode::NOT_FOUND.into_response();
    }

    let can_access_group = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM chat_member
            WHERE chat_id = $1
              AND user_id = $2
        )
        "#,
    )
    .bind(chat_id)
    .bind(current_user_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !can_access_group {
        return StatusCode::FORBIDDEN.into_response();
    }

    let member_ids = sqlx::query_scalar::<_, i32>(
        r#"
        SELECT user_id
        FROM chat_member
        WHERE chat_id = $1
        ORDER BY joined_at ASC
        "#,
    )
    .bind(chat_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    Json(member_ids).into_response()
}

#[allow(dead_code)]
pub async fn publish_chat(pool: &PgPool, channel: &str, payload: &str) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify($1, $2)")
        .bind(channel)
        .bind(payload)
        .execute(pool)
        .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn subscribe_chat(
    pool: PgPool,
    channel: &str,
    tx: tokio::sync::broadcast::Sender<String>,
) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen(channel).await?;

    loop {
        let notification = listener.recv().await?;
        let _ = tx.send(notification.payload().to_owned());
    }
}