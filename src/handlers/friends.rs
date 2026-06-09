#[derive(sqlx::FromRow)]
#[allow(dead_code)]
pub struct PendingFriendRequest {
    pub request_id: i32,
    pub sender_id: i32,
    pub receiver_id: i32,
}

#[derive(sqlx::FromRow)]
pub struct RelationshipRow {
    pub request_id: i32,
    pub user_id: i32,
    pub user_public_id: i64,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub relationship_kind: String,
    pub direction: String,
}

pub struct RelationshipUser {
    pub user_id: i32,
    pub public_id: i64,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
}

pub struct IncomingFriendRequestView {
    pub request_id: i32,
    pub sender_id: i32,
    pub sender_public_id: i64,
    pub sender_username: String,
    pub sender_profile_photo_url: String,
    pub sender_profile_photo_style: String,
}

#[allow(dead_code)]
pub struct OutgoingFriendRequestView {
    pub request_id: i32,
    pub receiver_id: i32,
    pub receiver_public_id: i64,
    pub receiver_username: String,
    pub receiver_profile_photo_url: String,
    pub receiver_profile_photo_style: String,
}

#[derive(sqlx::FromRow)]
pub struct FriendRecommendationView {
    pub user_id: i32,
    pub public_id: i64,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub mutual_friends_count: i64,
}

#[derive(Deserialize)]
pub struct FriendLookupForm {
    pub user_lookup: String,
}

#[derive(Template)]
#[template(path = "friends.html")]
#[allow(dead_code)]
pub struct FriendsTemplate {
    pub title: String,
    pub id: i64,
    pub user_id: i32,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub pending_requests: Vec<PendingFriendRequest>,
    pub incoming_requests: Vec<IncomingFriendRequestView>,
    pub outgoing_requests: Vec<OutgoingFriendRequestView>,
    pub friend_recommendations: Vec<FriendRecommendationView>,
    pub friendships: Vec<RelationshipUser>,
    pub blocked_users: Vec<RelationshipUser>,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
}

pub async fn friends(session: Session, State(pool): State<PgPool>) -> impl IntoResponse {
    let user_id = session.get::<i32>("id").await.ok().flatten();
    let public_user_id = session_public_user_id(&session).await.unwrap_or(0);
    let is_moderator = load_is_moderator(&pool, user_id).await;
    let (unread_notifications_count, notifications) =
        load_header_notifications(&pool, user_id).await;

    let (
        pending_requests,
        incoming_requests,
        outgoing_requests,
        friend_recommendations,
        friendships,
        blocked_users,
    ) = if let Some(current_user_id) = user_id {
        let relationship_rows = sqlx::query_as::<_, RelationshipRow>(
            r#"
                SELECT
                    r.friendship_id AS request_id,
                    CASE
                        WHEN r.sender_id = $1 THEN r.receiver_id
                        ELSE r.sender_id
                    END AS user_id,
                    u.public_id AS user_public_id,
                    u.username,
                    COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                    COALESCE(u.profile_photo_style, '') AS profile_photo_style,
                    LOWER(COALESCE(r.status, '')) AS relationship_kind,
                    CASE
                        WHEN r.sender_id = $1 THEN 'outgoing'
                        ELSE 'incoming'
                    END AS direction
                FROM relationship r
                JOIN users u ON u.id = CASE
                    WHEN r.sender_id = $1 THEN r.receiver_id
                    ELSE r.sender_id
                END
                WHERE r.sender_id = $1 OR r.receiver_id = $1
                ORDER BY r.friendship_id DESC
                "#,
        )
        .bind(current_user_id)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

        let mut pending_requests = Vec::new();
        let mut incoming_requests = Vec::new();
        let mut outgoing_requests = Vec::new();
        let mut friendships = Vec::new();
        let mut blocked_users = Vec::new();

        for row in relationship_rows {
            match (row.relationship_kind.as_str(), row.direction.as_str()) {
                ("friend", _) | ("friends", _) | ("friendship", _) | ("accepted", _) => friendships
                    .push(RelationshipUser {
                        user_id: row.user_id,
                        public_id: row.user_public_id,
                        username: row.username,
                        profile_photo_url: row.profile_photo_url.clone(),
                        profile_photo_style: row.profile_photo_style.clone(),
                    }),
                ("pending", "incoming")
                | ("request", "incoming")
                | ("requested", "incoming")
                | ("friend_request", "incoming") => {
                    pending_requests.push(PendingFriendRequest {
                        request_id: row.request_id,
                        sender_id: row.user_id,
                        receiver_id: current_user_id,
                    });
                    incoming_requests.push(IncomingFriendRequestView {
                        request_id: row.request_id,
                        sender_id: row.user_id,
                        sender_public_id: row.user_public_id,
                        sender_username: row.username,
                        sender_profile_photo_url: row.profile_photo_url.clone(),
                        sender_profile_photo_style: row.profile_photo_style.clone(),
                    });
                }
                ("pending", "outgoing")
                | ("request", "outgoing")
                | ("requested", "outgoing")
                | ("friend_request", "outgoing") => {
                    outgoing_requests.push(OutgoingFriendRequestView {
                        request_id: row.request_id,
                        receiver_id: row.user_id,
                        receiver_public_id: row.user_public_id,
                        receiver_username: row.username,
                        receiver_profile_photo_url: row.profile_photo_url.clone(),
                        receiver_profile_photo_style: row.profile_photo_style.clone(),
                    });
                }
                ("blocked", _) | ("block", _) => blocked_users.push(RelationshipUser {
                    user_id: row.user_id,
                    public_id: row.user_public_id,
                    username: row.username,
                    profile_photo_url: row.profile_photo_url,
                    profile_photo_style: row.profile_photo_style,
                }),
                _ => {}
            }
        }

        (
                pending_requests,
                incoming_requests,
                outgoing_requests,
                sqlx::query_as::<_, FriendRecommendationView>(
                    r#"
                    WITH friend_edges AS (
                        SELECT sender_id AS user_id, receiver_id AS friend_id
                        FROM relationship
                        WHERE LOWER(COALESCE(status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
                        UNION
                        SELECT receiver_id AS user_id, sender_id AS friend_id
                        FROM relationship
                        WHERE LOWER(COALESCE(status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
                    ),
                    my_friends AS (
                        SELECT friend_id
                        FROM friend_edges
                        WHERE user_id = $1
                    ),
                    candidate_mutuals AS (
                        SELECT
                            e2.friend_id AS candidate_id,
                            mf.friend_id AS mutual_friend_id
                        FROM my_friends mf
                        JOIN friend_edges e2
                          ON e2.user_id = mf.friend_id
                        WHERE e2.friend_id <> $1
                          AND NOT EXISTS (
                              SELECT 1
                              FROM my_friends mine
                              WHERE mine.friend_id = e2.friend_id
                          )
                    ),
                    latest_rel AS (
                        SELECT DISTINCT ON (
                            LEAST(sender_id, receiver_id),
                            GREATEST(sender_id, receiver_id)
                        )
                            sender_id,
                            receiver_id,
                            LOWER(COALESCE(status, '')) AS status
                        FROM relationship
                        WHERE sender_id = $1 OR receiver_id = $1
                        ORDER BY
                            LEAST(sender_id, receiver_id),
                            GREATEST(sender_id, receiver_id),
                            friendship_id DESC
                    )
                    SELECT
                        u.id AS user_id,
                        u.public_id,
                        u.username,
                        COALESCE(NULLIF(u.profile_photo_url, ''), '/public/avatar.webp') AS profile_photo_url,
                        COALESCE(u.profile_photo_style, '') AS profile_photo_style,
                        COUNT(DISTINCT cm.mutual_friend_id)::BIGINT AS mutual_friends_count
                    FROM candidate_mutuals cm
                    JOIN users u ON u.id = cm.candidate_id
                    LEFT JOIN latest_rel lr
                      ON LEAST(lr.sender_id, lr.receiver_id) = LEAST($1, u.id)
                     AND GREATEST(lr.sender_id, lr.receiver_id) = GREATEST($1, u.id)
                    WHERE u.id <> $1
                      AND (
                          lr.status IS NULL
                          OR lr.status IN ('rejected', 'declined', 'cancelled')
                      )
                    GROUP BY u.id, u.public_id, u.username, u.profile_photo_url, u.profile_photo_style
                    ORDER BY mutual_friends_count DESC, LOWER(u.username) ASC
                    LIMIT 30
                    "#,
                )
                .bind(current_user_id)
                .fetch_all(&pool)
                .await
                .unwrap_or_default(),
                friendships,
                blocked_users,
            )
    } else {
        (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    };

    let template = FriendsTemplate {
        title: "Friends".to_string(),
        id: public_user_id,
        user_id: user_id.unwrap_or(0),
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username: session_string(&session, "username", "").await,
        profile_photo_url: session_string(&session, "profile_photo_url", "/public/avatar.webp")
            .await,
        profile_photo_style: session_string(&session, "profile_photo_style", "").await,
        pending_requests,
        incoming_requests,
        outgoing_requests,
        friend_recommendations,
        friendships,
        blocked_users,
        unread_notifications_count,
        notifications,
    };
    render_template_response(&template)
}

async fn send_friend_request_to_user(pool: &PgPool, current_user_id: i32, target_user_id: i32) {
    if current_user_id == target_user_id || target_user_id <= 0 {
        return;
    }

    let target_is_instavox_team = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE id = $1
              AND LOWER(username) = LOWER($2)
        )
        "#,
    )
    .bind(target_user_id)
    .bind(MODERATOR_TEAM_USERNAME)
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if target_is_instavox_team {
        return;
    }

    let send_result = sqlx::query_scalar::<_, bool>(
        r#"
        WITH latest_relationship AS (
            SELECT
                r.friendship_id,
                LOWER(COALESCE(r.status, '')) AS relationship_kind
            FROM relationship r
            WHERE (r.sender_id = $1 AND r.receiver_id = $2)
               OR (r.sender_id = $2 AND r.receiver_id = $1)
            ORDER BY r.friendship_id DESC
            LIMIT 1
        ),
        revived AS (
            UPDATE relationship r
            SET sender_id = $1,
                receiver_id = $2,
                status = 'pending',
                modified_at = NOW()
            WHERE r.friendship_id = (
                SELECT friendship_id
                FROM latest_relationship
                WHERE relationship_kind IN ('rejected', 'declined', 'cancelled')
            )
              AND NOT EXISTS (
                  SELECT 1
                  FROM latest_relationship rel
                  WHERE rel.relationship_kind IN (
                      'friend',
                      'friends',
                      'friendship',
                      'accepted',
                      'pending',
                      'blocked'
                  )
              )
            RETURNING r.friendship_id
        ),
        next_id AS (
            SELECT COALESCE(MAX(friendship_id), 0) + 1 AS friendship_id
            FROM relationship
        ),
        inserted AS (
            INSERT INTO relationship (friendship_id, sender_id, receiver_id, status, created_at, modified_at)
            SELECT next_id.friendship_id, $1, $2, 'pending', NOW(), NOW()
            FROM next_id
            WHERE $1 <> $2
              AND NOT EXISTS (
                  SELECT 1
                  FROM latest_relationship rel
                  WHERE rel.relationship_kind IN (
                      'friend',
                      'friends',
                      'friendship',
                      'accepted',
                      'pending',
                      'blocked'
                  )
              )
              AND NOT EXISTS (SELECT 1 FROM revived)
            RETURNING friendship_id
        )
        SELECT EXISTS(SELECT 1 FROM revived) OR EXISTS(SELECT 1 FROM inserted) AS did_send
        "#,
    )
    .bind(current_user_id)
    .bind(target_user_id)
    .fetch_one(pool)
    .await;

    match send_result {
        Ok(did_send) => {
            if !did_send {
                tracing::info!(
                    "send_friend_request skipped for pair ({}, {}): existing relationship blocks it",
                    current_user_id,
                    target_user_id
                );
            } else if let Some((sender_username, _sender_public_id)) =
                load_user_identity(pool, current_user_id).await
            {
                let body = format!("@{} sent you a friend request", sender_username);
                let link_url = local_user_profile_path(&sender_username);
                if let Err(err) = create_notification(
                    pool,
                    target_user_id,
                    "friend_request",
                    "New friend request",
                    &body,
                    &link_url,
                )
                .await
                {
                    tracing::warn!("send_friend_request notification failed: {}", err);
                }
            }
        }
        Err(err) => {
            tracing::warn!("send_friend_request failed: {}", err);
        }
    }
}

pub async fn send_friend_request(
    session: Session,
    State(pool): State<PgPool>,
    Path(target_user_id): Path<i32>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    send_friend_request_to_user(&pool, current_user_id, target_user_id).await;
    Redirect::to("/friends").into_response()
}

pub async fn send_friend_request_by_lookup(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<FriendLookupForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let lookup = payload.user_lookup.trim();
    if lookup.is_empty() {
        return Redirect::to("/friends").into_response();
    }

    let target_user_id = if let Ok(public_id) = lookup.parse::<i64>() {
        sqlx::query_scalar::<_, i32>(
            r#"
            SELECT id
            FROM users
            WHERE public_id = $1
            "#,
        )
        .bind(public_id)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
    } else {
        sqlx::query_scalar::<_, i32>(
            r#"
            SELECT id
            FROM users
            WHERE LOWER(username) = LOWER($1)
               OR LOWER(COALESCE(preferred_username, '')) = LOWER($1)
            ORDER BY id ASC
            LIMIT 1
            "#,
        )
        .bind(lookup)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
    };

    if let Some(target_user_id) = target_user_id {
        send_friend_request_to_user(&pool, current_user_id, target_user_id).await;
    }

    Redirect::to("/friends").into_response()
}

pub async fn accept_friend_request(
    session: Session,
    State(pool): State<PgPool>,
    Path(request_id): Path<i32>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let accept_result = sqlx::query_scalar::<_, i32>(
        r#"
        UPDATE relationship
        SET status = 'friendship',
            modified_at = NOW()
        WHERE friendship_id = $1
          AND receiver_id = $2
          AND LOWER(COALESCE(status, '')) IN ('pending', 'request', 'requested', 'friend_request')
        RETURNING sender_id
        "#,
    )
    .bind(request_id)
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await;

    match accept_result {
        Ok(Some(sender_id)) => {
            if let Some((receiver_username, _receiver_public_id)) =
                load_user_identity(&pool, current_user_id).await
            {
                let body = format!("@{} accepted your friend request", receiver_username);
                let link_url = local_user_profile_path(&receiver_username);
                if let Err(err) = create_notification(
                    &pool,
                    sender_id,
                    "friend_request_accepted",
                    "Friend request accepted",
                    &body,
                    &link_url,
                )
                .await
                {
                    tracing::warn!("accept_friend_request notification failed: {}", err);
                }
            }
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("accept_friend_request failed: {}", err);
        }
    }

    Redirect::to("/friends").into_response()
}

pub async fn reject_friend_request(
    session: Session,
    State(pool): State<PgPool>,
    Path(request_id): Path<i32>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let reject_result = sqlx::query_scalar::<_, i32>(
        r#"
        UPDATE relationship
        SET status = 'rejected',
            modified_at = NOW()
        WHERE friendship_id = $1
          AND receiver_id = $2
          AND LOWER(COALESCE(status, '')) IN ('pending', 'request', 'requested', 'friend_request')
        RETURNING sender_id
        "#,
    )
    .bind(request_id)
    .bind(current_user_id)
    .fetch_optional(&pool)
    .await;

    match reject_result {
        Ok(Some(sender_id)) => {
            if let Some((receiver_username, _receiver_public_id)) =
                load_user_identity(&pool, current_user_id).await
            {
                let body = format!("@{} rejected your friend request", receiver_username);
                let link_url = local_user_profile_path(&receiver_username);
                if let Err(err) = create_notification(
                    &pool,
                    sender_id,
                    "friend_request_rejected",
                    "Friend request rejected",
                    &body,
                    &link_url,
                )
                .await
                {
                    tracing::warn!("reject_friend_request notification failed: {}", err);
                }
            }
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("reject_friend_request failed: {}", err);
        }
    }

    Redirect::to("/friends").into_response()
}