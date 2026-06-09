async fn notify_profile_update_to_federation(pool: &PgPool, user_id: i32, context: &str) {
    if let Err(err) =
        crate::federation::send_profile_update_to_mastodon_followers(pool, user_id).await
    {
        tracing::warn!("{} profile ActivityPub update failed: {}", context, err);
    }
}



#[derive(Deserialize)]
pub struct UpdateFederationForm {
    pub federated: String,
    pub display_name_mode: String,
}

pub async fn settings_update_federation(
    session: Session,
    State(pool): State<PgPool>,
    Form(payload): Form<UpdateFederationForm>,
) -> impl IntoResponse {
    let Some(current_user_id) = session_user_id(&session).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(federation_enabled) = parse_federation_setting(&payload.federated) else {
        return Redirect::to("/settings?federation_status=invalid").into_response();
    };
    let Some(display_name_mode) = parse_federation_display_name_mode(&payload.display_name_mode)
    else {
        return Redirect::to("/settings?federation_status=invalid").into_response();
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE users
        SET federation_enabled = $1,
            federation_display_name_mode = $2
        WHERE id = $3
        "#,
    )
    .bind(federation_enabled)
    .bind(display_name_mode)
    .bind(current_user_id)
    .execute(&pool)
    .await
    {
        tracing::warn!("settings_update_federation update failed: {}", err);
        return Redirect::to("/settings?federation_status=update_error").into_response();
    }

    if federation_enabled {
        notify_profile_update_to_federation(&pool, current_user_id, "settings_update_federation")
            .await;
    }

    if federation_enabled {
        Redirect::to("/settings?federation_status=changed_enabled").into_response()
    } else {
        Redirect::to("/settings?federation_status=changed_disabled").into_response()
    }
}