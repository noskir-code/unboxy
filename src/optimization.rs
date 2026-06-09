pub async fn compress_existing_uploaded_images(pool: &PgPool) {
    let mut urls = BTreeSet::new();

    let post_urls = sqlx::query_scalar::<_, String>(
        "SELECT image_url FROM post_image WHERE image_url LIKE '/public/uploads/%'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    urls.extend(post_urls);

    let user_upload_urls = sqlx::query_scalar::<_, String>(
        "SELECT file_url FROM user_image_upload WHERE file_url LIKE '/public/uploads/%'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    urls.extend(user_upload_urls);

    let user_photo_urls = sqlx::query_scalar::<_, String>(
        r#"
        SELECT url
        FROM (
            SELECT profile_photo_url AS url FROM users
            UNION
            SELECT background_photo_url AS url FROM users
        ) image_urls
        WHERE url LIKE '/public/uploads/%'
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    urls.extend(user_photo_urls);

    let community_photo_urls = sqlx::query_scalar::<_, String>(
        "SELECT profile_photo_url FROM community_page WHERE profile_photo_url LIKE '/public/uploads/%'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    urls.extend(community_photo_urls);

    let message_attachment_urls = sqlx::query_scalar::<_, String>(
        r#"
        SELECT file_path
        FROM message_attachment
        WHERE file_path LIKE '/public/uploads/%'
          AND LOWER(COALESCE(mime_type, '')) LIKE 'image/%'
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    urls.extend(message_attachment_urls);

    for old_url in urls {
        let new_url = match compress_public_upload_to_jpeg(old_url.clone()).await {
            Ok(Some(new_url)) => new_url,
            Ok(None) => continue,
            Err(err) => {
                tracing::warn!("existing image compression failed for {}: {}", old_url, err);
                continue;
            }
        };

        if new_url == old_url {
            continue;
        }

        if let Err(err) =
            sqlx::query("UPDATE post_image SET image_url = $1, mime_type = $2 WHERE image_url = $3")
                .bind(&new_url)
                .bind(COMPRESSED_IMAGE_MIME)
                .bind(&old_url)
                .execute(pool)
                .await
        {
            tracing::warn!("post_image compression URL update failed: {}", err);
        }
        if let Err(err) =
            sqlx::query("UPDATE user_image_upload SET file_url = $1 WHERE file_url = $2")
                .bind(&new_url)
                .bind(&old_url)
                .execute(pool)
                .await
        {
            tracing::warn!("user_image_upload compression URL update failed: {}", err);
        }
        if let Err(err) = sqlx::query(
            r#"
            UPDATE users
            SET profile_photo_url = CASE WHEN profile_photo_url = $2 THEN $1 ELSE profile_photo_url END,
                background_photo_url = CASE WHEN background_photo_url = $2 THEN $1 ELSE background_photo_url END
            WHERE profile_photo_url = $2 OR background_photo_url = $2
            "#,
        )
        .bind(&new_url)
        .bind(&old_url)
        .execute(pool)
        .await
        {
            tracing::warn!("users compression URL update failed: {}", err);
        }
        if let Err(err) = sqlx::query(
            "UPDATE community_page SET profile_photo_url = $1 WHERE profile_photo_url = $2",
        )
        .bind(&new_url)
        .bind(&old_url)
        .execute(pool)
        .await
        {
            tracing::warn!("community_page compression URL update failed: {}", err);
        }
        if let Err(err) = sqlx::query(
            "UPDATE message_attachment SET file_path = $1, mime_type = $2 WHERE file_path = $3",
        )
        .bind(&new_url)
        .bind(COMPRESSED_IMAGE_MIME)
        .bind(&old_url)
        .execute(pool)
        .await
        {
            tracing::warn!("message_attachment compression URL update failed: {}", err);
        }
    }
}