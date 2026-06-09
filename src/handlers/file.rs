fn normalize_uploaded_filename_stem(raw: Option<&str>) -> String {
    let file_name = raw
        .and_then(|name| FsPath::new(name).file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let mut cleaned: String = file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        return "image".to_string();
    }
    if cleaned.len() > 48 {
        cleaned.truncate(48);
    }
    cleaned
}