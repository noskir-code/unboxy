pub async fn follow_user(
    session: Session,
    State(pool): State<PgPool>,
    Path(target_user_id): Path<i32>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if target_user_id <= 0 || target_user_id == current_user_id {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let target_exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE id = $1
        )
        "#,
    )
    .bind(target_user_id)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if !target_exists {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    let follow_result = sqlx::query_scalar::<_, bool>(
        r#"
        WITH latest_pair AS (
            SELECT LOWER(COALESCE(r.status, '')) AS relationship_kind
            FROM relationship r
            WHERE (
                (r.sender_id = $1 AND r.receiver_id = $2)
                OR
                (r.sender_id = $2 AND r.receiver_id = $1)
            )
            ORDER BY r.friendship_id DESC
            LIMIT 1
        ),
        latest_outgoing AS (
            SELECT LOWER(COALESCE(r.status, '')) AS relationship_kind
            FROM relationship r
            WHERE r.sender_id = $1
              AND r.receiver_id = $2
            ORDER BY r.friendship_id DESC
            LIMIT 1
        ),
        next_id AS (
            SELECT COALESCE(MAX(friendship_id), 0) + 1 AS friendship_id
            FROM relationship
        ),
        inserted AS (
            INSERT INTO relationship (friendship_id, sender_id, receiver_id, status, created_at, modified_at)
            SELECT next_id.friendship_id, $1, $2, 'follow', NOW(), NOW()
            FROM next_id
            WHERE $1 <> $2
              AND NOT EXISTS (
                  SELECT 1
                  FROM latest_pair lp
                  WHERE lp.relationship_kind IN (
                      'blocked',
                      'block',
                      'friend',
                      'friends',
                      'friendship',
                      'accepted'
                  )
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM latest_outgoing lo
                  WHERE lo.relationship_kind IN ('follow', 'following', 'follower')
              )
            RETURNING friendship_id
        )
        SELECT EXISTS(SELECT 1 FROM inserted)
        "#,
    )
    .bind(current_user_id)
    .bind(target_user_id)
    .fetch_one(&pool)
    .await;

    match follow_result {
        Ok(true) => {
            if let Some((sender_username, _sender_public_id)) =
                load_user_identity(&pool, current_user_id).await
            {
                let body = format!("@{} started following you", sender_username);
                let link_url = local_user_profile_path(&sender_username);
                if let Err(err) = create_notification(
                    &pool,
                    target_user_id,
                    "new_follower",
                    "New follower",
                    &body,
                    &link_url,
                )
                .await
                {
                    tracing::warn!("follow_user notification failed: {}", err);
                }
            }
        }
        Ok(false) => {}
        Err(err) => tracing::warn!("follow_user failed: {}", err),
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn unfollow_user(
    session: Session,
    State(pool): State<PgPool>,
    Path(target_user_id): Path<i32>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    if target_user_id <= 0 || target_user_id == current_user_id {
        let redirect_to = redirect_back_path(&headers);
        return Redirect::to(&redirect_to).into_response();
    }

    if let Err(err) = sqlx::query(
        r#"
        WITH latest_follow AS (
            SELECT friendship_id
            FROM relationship
            WHERE sender_id = $1
              AND receiver_id = $2
              AND LOWER(COALESCE(status, '')) IN ('follow', 'following', 'follower')
            ORDER BY friendship_id DESC
            LIMIT 1
        )
        UPDATE relationship r
        SET status = 'removed',
            modified_at = NOW()
        WHERE r.friendship_id = (
            SELECT friendship_id
            FROM latest_follow
        )
        "#,
    )
    .bind(current_user_id)
    .bind(target_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("unfollow_user failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}