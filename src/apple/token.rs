//! Developer-token discovery from Apple's public web player.

use regex_lite::Regex;
use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

const BUNDLE: &str = r#"/((?:assets/)?index[~-][^/"\s]+\.js)"#;
const TOKEN: &str = r#""(eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+)""#;

fn bundle_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(BUNDLE).expect("valid bundle pattern"))
}

fn token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(TOKEN).expect("valid token pattern"))
}

/// Fetch the player page, locate its bundle and extract a JWT-shaped token.
pub async fn discover(http: &Client, site_base: &str) -> Result<String, &'static str> {
    let page = http
        .get(site_base)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| "web player page request failed")?
        .error_for_status()
        .map_err(|_| "web player page rejected")?
        .text()
        .await
        .map_err(|_| "web player page was unreadable")?;

    let path = bundle_pattern()
        .captures(&page)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_string())
        .ok_or("web player bundle was not found")?;

    let script = http
        .get(format!("{site_base}/{path}"))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| "web player bundle request failed")?
        .error_for_status()
        .map_err(|_| "web player bundle rejected")?
        .text()
        .await
        .map_err(|_| "web player bundle was unreadable")?;

    token_pattern()
        .captures(&script)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_string())
        .ok_or("developer token was not found")
}
