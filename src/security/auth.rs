use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct ActorKeypair {
    pub public_key: String,
    pub private_key: String,
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let mut hasher = DefaultHasher::new();
    password.hash(&mut hasher);
    Ok(format!("dev-hash-{:x}", hasher.finish()))
}

pub fn verify_password(password: &str, stored_hash: &str) -> Result<bool, String> {
    Ok(hash_password(password)? == stored_hash)
}

pub fn validate_secure_password(password: &str) -> Result<(), String> {
    if password.len() >= 8 {
        Ok(())
    } else {
        Err("password must be at least 8 characters".to_string())
    }
}

pub fn secure_password_requirements_text() -> String {
    "Password must be at least 8 characters.".to_string()
}

pub fn is_valid_signup_email(email: &str) -> bool {
    let email = email.trim();
    email.contains('@') && email.contains('.') && !email.contains(char::is_whitespace)
}

pub fn generate_actor_keypair() -> Result<ActorKeypair, String> {
    Ok(ActorKeypair {
        public_key: "dev-public-key".to_string(),
        private_key: "dev-private-key".to_string(),
    })
}

pub async fn invalidate_remember_token_from_headers(
    _pool: &sqlx::PgPool,
    _headers: &axum::http::HeaderMap,
) {
}

pub fn clear_remember_cookie_value() -> String {
    "remember=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax".to_string()
}

pub async fn send_smtp_test_email(recipient_email: &str) -> Result<(), String> {
    let subject = "Instavox SMTP test email";
    let body = format!(
        "This is a test email sent by Instavox at {} UTC.\n\nIf you receive this, SMTP is configured correctly.",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
    );
    send_smtp_email(recipient_email, subject, &body).await
}