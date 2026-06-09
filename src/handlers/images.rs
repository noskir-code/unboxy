fn parse_crop_float(raw: Option<&str>, min: f32, max: f32, fallback: f32) -> f32 {
    let parsed = raw
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(fallback);
    parsed.clamp(min, max)
}

fn build_image_crop_style(crop_x: f32, crop_y: f32, crop_zoom: f32) -> String {
    format!(
        "object-position: {:.2}% {:.2}%; transform: scale({:.4}); transform-origin: center center;",
        crop_x.clamp(0.0, 100.0),
        crop_y.clamp(0.0, 100.0),
        crop_zoom.clamp(1.0, 3.0)
    )
}

fn crop_style_from_form(form: &SelectUploadedImageForm) -> String {
    let explicit_apply = form
        .crop_apply
        .as_deref()
        .map(|value| value.trim())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes" | "on"));
    let has_crop_values =
        form.crop_x.is_some() || form.crop_y.is_some() || form.crop_zoom.is_some();
    let apply_crop = explicit_apply || has_crop_values;
    if !apply_crop {
        return String::new();
    }

    let crop_x = parse_crop_float(form.crop_x.as_deref(), 0.0, 100.0, 50.0);
    let crop_y = parse_crop_float(form.crop_y.as_deref(), 0.0, 100.0, 50.0);
    let crop_zoom = parse_crop_float(form.crop_zoom.as_deref(), 1.0, 3.0, 1.0);
    build_image_crop_style(crop_x, crop_y, crop_zoom)
}

fn detect_image_extension(file_name: Option<&str>, content_type: Option<&str>) -> Option<String> {
    let from_mime = match content_type {
        Some("image/jpeg") | Some("image/jpg") => Some("jpg"),
        Some("image/png") => Some("png"),
        Some("image/webp") => Some("webp"),
        Some("image/gif") => Some("gif"),
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
        Some("webp") => Some("webp".to_string()),
        Some("gif") => Some("gif".to_string()),
        Some("avif") => Some("avif".to_string()),
        Some("bmp") => Some("bmp".to_string()),
        _ => None,
    }
}

async fn read_uploaded_image(
    mut multipart: Multipart,
) -> Result<(Option<String>, Option<String>, Vec<u8>), String> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| "Invalid upload payload".to_string())?
    {
        if field.name() != Some("file") {
            continue;
        }
        let file_name = field.file_name().map(str::to_string);
        let content_type = field.content_type().map(str::to_string);
        let bytes = field
            .bytes()
            .await
            .map_err(|_| "Failed to read uploaded file".to_string())?;
        return Ok((file_name, content_type, bytes.to_vec()));
    }
    Err("Please choose an image file to upload".to_string())
}