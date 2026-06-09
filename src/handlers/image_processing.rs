use axum::extract::Multipart;

pub async fn compress_upload_to_jpeg(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        Err("Uploaded file is empty".to_string())
    } else {
        Ok(bytes)
    }
}

pub async fn read_uploaded_image(
    mut multipart: Multipart,
) -> Result<(String, String, Vec<u8>), String> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| "Invalid upload payload".to_string())?
    {
        let file_name = field
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| "upload".to_string());
        let content_type = field
            .content_type()
            .map(str::to_string)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|_| "Failed to read upload".to_string())?
            .to_vec();

        if !bytes.is_empty() {
            return Ok((file_name, content_type, bytes));
        }
    }

    Err("No image file was uploaded".to_string())
}
