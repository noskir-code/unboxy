

pub async fn remove_friend(
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
        WITH latest_friendship AS (
            SELECT friendship_id
            FROM relationship
            WHERE (
                (sender_id = $1 AND receiver_id = $2)
                OR
                (sender_id = $2 AND receiver_id = $1)
            )
              AND LOWER(COALESCE(status, '')) IN ('friend', 'friends', 'friendship', 'accepted')
            ORDER BY friendship_id DESC
            LIMIT 1
        )
        UPDATE relationship r
        SET status = 'removed',
            modified_at = NOW()
        WHERE r.friendship_id = (
            SELECT friendship_id
            FROM latest_friendship
        )
        "#,
    )
    .bind(current_user_id)
    .bind(target_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("remove_friend failed: {}", err);
    }

    let redirect_to = redirect_back_path(&headers);
    Redirect::to(&redirect_to).into_response()
}

pub async fn logout(
    session: Session,
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = session.delete().await {
        tracing::warn!("logout session delete failed: {}", err);
    }
    invalidate_remember_token_from_headers(&pool, &headers).await;

    let mut response = Redirect::to("/login").into_response();
    if let Ok(cookie_header) = HeaderValue::from_str(&clear_remember_cookie_value()) {
        response.headers_mut().append(SET_COOKIE, cookie_header);
    }
    response
}