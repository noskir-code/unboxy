#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate {
    pub title: String,
    pub id: i64,
    pub is_moderator: bool,
    pub local_profile_domain: String,
    pub username: String,
    pub profile_photo_url: String,
    pub profile_photo_style: String,
    pub unread_notifications_count: i64,
    pub notifications: Vec<HeaderNotificationView>,
    pub error_code: u16,
    pub error_name: String,
    pub error_message: String,
}

fn custom_error_page_copy(status: StatusCode) -> (&'static str, &'static str) {
    match status {
        StatusCode::BAD_REQUEST => (
            "Bad Request",
            "The request format is invalid or incomplete. Please go back and try again.",
        ),
        StatusCode::UNAUTHORIZED => (
            "Unauthorized",
            "You need to be logged in to access this page.",
        ),
        StatusCode::FORBIDDEN => (
            "Forbidden",
            "You do not have permission to access this resource.",
        ),
        StatusCode::NOT_FOUND => (
            "Page Not Found",
            "The page you requested does not exist or may have been moved.",
        ),
        StatusCode::INTERNAL_SERVER_ERROR => (
            "Internal Server Error",
            "Something went wrong on our side. Please try again in a moment.",
        ),
        StatusCode::BAD_GATEWAY => (
            "Bad Gateway",
            "The upstream service returned an invalid response.",
        ),
        StatusCode::SERVICE_UNAVAILABLE => (
            "Service Unavailable",
            "The service is temporarily unavailable. Please try again shortly.",
        ),
        StatusCode::GATEWAY_TIMEOUT => (
            "Gateway Timeout",
            "The server took too long to respond. Please retry.",
        ),
        _ => ("Unexpected Error", "An unexpected error occurred."),
    }
}

fn is_custom_error_page_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_html_page_request(method: &Method, accept_header: &str) -> bool {
    if *method != Method::GET {
        return false;
    }
    let normalized = accept_header.to_ascii_lowercase();
    normalized.is_empty() || normalized.contains("text/html") || normalized.contains("*/*")
}

fn should_skip_custom_error_page(path: &str) -> bool {
    path.starts_with("/v1/")
        || path.starts_with("/ws/")
        || path.starts_with("/ap/")
        || path.starts_with("/health/")
        || path.starts_with("/assets/")
        || path.starts_with("/public/")
}

pub async fn render_custom_error_pages(
    State(pool): State<PgPool>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let accept_header = request
        .headers()
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let session = request.extensions().get::<Session>().cloned();

    let response = next.run(request).await;
    let status = response.status();

    if !is_custom_error_page_status(status)
        || should_skip_custom_error_page(&path)
        || !is_html_page_request(&method, &accept_header)
    {
        return response;
    }

    let mut public_id = 0_i64;
    let mut username = String::new();
    let mut profile_photo_url = DEFAULT_PROFILE_PHOTO_URL.to_string();
    let mut profile_photo_style = String::new();
    let mut is_moderator = false;
    let mut unread_notifications_count = 0_i64;
    let mut notifications: Vec<HeaderNotificationView> = Vec::new();

    if let Some(session) = session.as_ref() {
        let user_id = session_user_id(session).await.unwrap_or(0);
        public_id = session_public_user_id(session).await.unwrap_or(0);
        username = session_string(session, "username", "").await;
        profile_photo_url =
            session_string(session, "profile_photo_url", DEFAULT_PROFILE_PHOTO_URL).await;
        profile_photo_style = session_string(session, "profile_photo_style", "").await;
        if user_id > 0 {
            is_moderator = load_is_moderator(&pool, Some(user_id)).await;
            let (count, header_notifications) =
                load_header_notifications(&pool, Some(user_id)).await;
            unread_notifications_count = count;
            notifications = header_notifications;
        }
    }

    let (error_name, error_message) = custom_error_page_copy(status);
    let template = ErrorTemplate {
        title: format!("{} {}", status.as_u16(), error_name),
        id: public_id,
        is_moderator,
        local_profile_domain: local_profile_domain(),
        username,
        profile_photo_url,
        profile_photo_style,
        unread_notifications_count,
        notifications,
        error_code: status.as_u16(),
        error_name: error_name.to_string(),
        error_message: error_message.to_string(),
    };

    let mut custom_response = render_template_response(&template);
    *custom_response.status_mut() = status;
    custom_response
}