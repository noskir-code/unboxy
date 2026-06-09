pub mod handlers;
pub mod routes;
mod security;

const CANONICAL_INSTAVOX_BASE_URL: &str = "https://domain.com";
const CANONICAL_INSTAVOX_DOMAIN: &str = "domain.com";

fn main() {
    println!("Hello, world!");
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
