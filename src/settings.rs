//! Session settings and credential parsing.
//!
//! The host passes settings as a flat JSON string map and the credential as an
//! opaque string. This provider accepts a JSON credential object so callers can
//! persist both Apple tokens together, or a bare `media-user-token`.

use serde_json::Value;

pub const DEFAULT_API_BASE: &str = "https://amp-api.music.apple.com";
pub const DEFAULT_SITE_BASE: &str = "https://music.apple.com";

#[derive(Debug, Clone)]
pub struct Settings {
    pub api_base: String,
    pub site_base: String,
    pub storefront: String,
    pub language: String,
    pub developer_token: Option<String>,
    pub cover_size: String,
    pub allow_preview: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.to_string(),
            site_base: DEFAULT_SITE_BASE.to_string(),
            storefront: "us".to_string(),
            language: "en-US".to_string(),
            developer_token: None,
            cover_size: "1200".to_string(),
            allow_preview: true,
        }
    }
}

/// Credential split into the two Apple tokens this provider understands.
#[derive(Debug, Clone, Default)]
pub struct Credential {
    pub developer_token: Option<String>,
    pub media_user_token: Option<String>,
    pub storefront: Option<String>,
    pub language: Option<String>,
}

impl Settings {
    pub fn from_json(settings_json: Option<&str>) -> Self {
        let mut settings = Self::default();
        let Some(raw) = settings_json.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return settings;
        };
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
            return settings;
        };
        for (key, value) in map {
            let Value::String(value) = value else {
                continue;
            };
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match key.as_str() {
                "apiBase" | "api_base" => {
                    settings.api_base = value.trim_end_matches('/').to_string()
                }
                "siteBase" | "site_base" => {
                    settings.site_base = value.trim_end_matches('/').to_string()
                }
                "storefront" => settings.storefront = value.to_ascii_lowercase(),
                "language" => settings.language = value,
                "developerToken" | "developer_token" => settings.developer_token = Some(value),
                "coverSize" | "cover_size" => settings.cover_size = value,
                "allowPreview" | "allow_preview" => {
                    settings.allow_preview = !matches!(value.as_str(), "false" | "0" | "no")
                }
                _ => {}
            }
        }
        settings
    }
}

impl Credential {
    pub fn parse(raw_credential: Option<&str>) -> Self {
        let mut credential = Self::default();
        let Some(raw) = raw_credential.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return credential;
        };

        if raw.starts_with('{') {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
                for (key, value) in map {
                    let Value::String(value) = value else {
                        continue;
                    };
                    let value = value.trim().to_string();
                    if value.is_empty() {
                        continue;
                    }
                    match key.as_str() {
                        "developerToken" | "developer_token" | "devToken" => {
                            credential.developer_token = Some(value)
                        }
                        "mediaUserToken" | "media_user_token" | "musicUserToken" | "token" => {
                            credential.media_user_token = Some(value)
                        }
                        "storefront" => credential.storefront = Some(value.to_ascii_lowercase()),
                        "language" => credential.language = Some(value),
                        _ => {}
                    }
                }
                return credential;
            }
        }

        credential.media_user_token = Some(raw.to_string());
        credential
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_settings_map() {
        let settings = Settings::from_json(Some(
            r#"{"storefront":"JP","language":"ja-JP","developerToken":"dev","allowPreview":"false"}"#,
        ));
        assert_eq!(settings.storefront, "jp");
        assert_eq!(settings.language, "ja-JP");
        assert_eq!(settings.developer_token.as_deref(), Some("dev"));
        assert!(!settings.allow_preview);
    }

    #[test]
    fn accepts_json_and_bare_credentials() {
        let json = Credential::parse(Some(
            r#"{"mediaUserToken":"media","developerToken":"dev","storefront":"GB"}"#,
        ));
        assert_eq!(json.media_user_token.as_deref(), Some("media"));
        assert_eq!(json.developer_token.as_deref(), Some("dev"));
        assert_eq!(json.storefront.as_deref(), Some("gb"));

        let bare = Credential::parse(Some("  media-only  "));
        assert_eq!(bare.media_user_token.as_deref(), Some("media-only"));
        assert!(bare.developer_token.is_none());

        assert!(Credential::parse(Some("   ")).media_user_token.is_none());
    }
}
