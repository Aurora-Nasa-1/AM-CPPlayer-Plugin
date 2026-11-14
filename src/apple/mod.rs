//! Minimal Apple Music Web API client.
//!
//! Only endpoints needed by the declared operations are implemented. Responses
//! stay as `serde_json::Value` and are mapped in [`parse`]; no response envelope
//! of the old plugin is reproduced.

pub mod parse;
pub mod token;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, COOKIE, ORIGIN, USER_AGENT};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;

use crate::error::{authentication, invalid_response, network, not_found, rate_limited, timeout};
use crate::models::Failure;
use crate::settings::Settings;

const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/126.0 Safari/537.36";
const MAX_PAGE: i32 = 500;
/// HLS playlists are tiny text documents; anything larger is not a playlist.
const MAX_PLAYLIST_BYTES: usize = 4 * 1024 * 1024;
const LICENSE_URL: &str =
    "https://play.itunes.apple.com/WebObjects/MZPlay.woa/wa/acquireWebPlaybackLicense";

#[derive(Clone)]
pub struct AppleClient {
    http: Client,
    settings: Settings,
    media_user_token: Option<String>,
    developer_token: Arc<tokio::sync::Mutex<Option<String>>>,
    cancelled: Arc<AtomicBool>,
}

impl AppleClient {
    pub fn new(
        settings: Settings,
        media_user_token: Option<String>,
        developer_token: Option<String>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, Failure> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent(USER_AGENT_VALUE)
            .build()
            .map_err(|_| network("Unable to initialize the HTTP client"))?;
        Ok(Self {
            http,
            settings,
            media_user_token,
            developer_token: Arc::new(tokio::sync::Mutex::new(developer_token)),
            cancelled,
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn is_logged_in(&self) -> bool {
        self.media_user_token.is_some()
    }

    fn origin(&self) -> &str {
        &self.settings.site_base
    }

    /// Resolve the developer token once per session from settings, credential
    /// or Apple's public web player.
    async fn developer_token(&self) -> Result<String, Failure> {
        let mut cached = self.developer_token.lock().await;
        if let Some(token) = cached.as_ref() {
            return Ok(token.clone());
        }
        let token = token::discover(&self.http, &self.settings.site_base)
            .await
            .map_err(|reason| {
                authentication(format!("Apple Music developer token unavailable: {reason}"))
            })?;
        *cached = Some(token.clone());
        Ok(token)
    }

    fn check_cancelled(&self) -> Result<(), Failure> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(Failure {
                category: "Runtime".to_string(),
                message: "Request cancelled".to_string(),
            })
        } else {
            Ok(())
        }
    }

    async fn headers(&self) -> Result<HeaderMap, Failure> {
        let mut headers = HeaderMap::new();
        let token = self.developer_token().await?;
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| authentication("Invalid developer token"))?;
        headers.insert(AUTHORIZATION, value);
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(self.origin()).unwrap_or(HeaderValue::from_static("*")),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        if let Some(media) = self.media_user_token.as_deref() {
            if let Ok(value) = HeaderValue::from_str(&format!("media-user-token={media}")) {
                headers.insert(COOKIE, value);
            }
        }
        Ok(headers)
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.settings.api_base, path)
        }
    }

    async fn send(
        &self,
        method: Method,
        url: String,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Value, Failure> {
        self.check_cancelled()?;
        let headers = self.headers().await?;
        let mut request = self.http.request(method, &url).headers(headers);
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                timeout("Apple Music request timed out")
            } else {
                network("Apple Music request failed")
            }
        })?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(not_found("Apple Music resource not found"));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(authentication("Apple Music rejected the credential"));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(rate_limited("Apple Music rate limit reached"));
        }
        if status.is_server_error() {
            return Err(network("Apple Music is temporarily unavailable"));
        }
        if !status.is_success() {
            return Err(invalid_response(format!(
                "Apple Music returned HTTP {}",
                status.as_u16()
            )));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| invalid_response("Apple Music returned malformed JSON"))?;
        if let Some(errors) = value.get("errors").and_then(Value::as_array) {
            if !errors.is_empty() {
                return Err(invalid_response("Apple Music reported an API error"));
            }
        }
        Ok(value)
    }

    async fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value, Failure> {
        self.send(Method::GET, self.url(path), query, None).await
    }

    // ------------------------------------------------------------------
    // Catalog reads
    // ------------------------------------------------------------------

    pub async fn search(
        &self,
        term: &str,
        types: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        let query = vec![
            ("term".to_string(), term.to_string()),
            ("types".to_string(), types.to_string()),
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!("/v1/catalog/{}/search", self.settings.storefront),
            &query,
        )
        .await
    }

    pub async fn get_song(&self, id: &str) -> Result<Value, Failure> {
        let query = vec![
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("include".to_string(), "artists,albums".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/songs/{}",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_song_lyrics(&self, id: &str) -> Result<Value, Failure> {
        let query = vec![("l".to_string(), self.settings.language.clone())];
        self.get(
            &format!(
                "/v1/catalog/{}/songs/{}/lyrics",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_album(&self, id: &str) -> Result<Value, Failure> {
        let query = vec![
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/albums/{}",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_album_tracks(
        &self,
        id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/albums/{}/tracks",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_playlist(&self, id: &str) -> Result<Value, Failure> {
        // Apple rejects `limit[tracks]` on some playlists (HTTP 400). The
        // metadata endpoint does not need it; tracks are paged separately.
        let query = vec![
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/playlists/{}",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_playlist_tracks(
        &self,
        id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/playlists/{}/tracks",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_artist(&self, id: &str) -> Result<Value, Failure> {
        // No relationships: `include=albums` makes the response shape depend
        // on catalog availability, and this operation only needs attributes.
        let query = vec![("l".to_string(), self.settings.language.clone())];
        self.get(
            &format!(
                "/v1/catalog/{}/artists/{}",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_artist_tracks(
        &self,
        id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/artists/{}/view/top-songs",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    pub async fn get_artist_albums(
        &self,
        id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!(
                "/v1/catalog/{}/artists/{}/albums",
                self.settings.storefront,
                encode(id)
            ),
            &query,
        )
        .await
    }

    // ------------------------------------------------------------------
    // Account and personal library
    // ------------------------------------------------------------------

    pub async fn get_account(&self) -> Result<Value, Failure> {
        let query = vec![("meta".to_string(), "subscription".to_string())];
        self.get("/v1/me/account", &query).await
    }

    pub async fn get_user_playlists(&self, limit: i32, offset: i32) -> Result<Value, Failure> {
        if !self.is_logged_in() {
            return Err(authentication(
                "Apple Music media user token is required for library access",
            ));
        }
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get("/v1/me/library/playlists", &query).await
    }

    /// Library playlists use their own path; catalog playlists do not.
    pub async fn get_library_playlist(&self, id: &str) -> Result<Value, Failure> {
        self.require_login()?;
        let query = vec![
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(&format!("/v1/me/library/playlists/{}", encode(id)), &query)
            .await
    }

    pub async fn get_library_playlist_tracks(
        &self,
        id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Value, Failure> {
        self.require_login()?;
        let query = vec![
            ("limit".to_string(), clamp_page(limit).to_string()),
            ("offset".to_string(), offset.max(0).to_string()),
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(
            &format!("/v1/me/library/playlists/{}/tracks", encode(id)),
            &query,
        )
        .await
    }

    pub async fn get_library_song(&self, id: &str) -> Result<Value, Failure> {
        self.require_login()?;
        let query = vec![
            ("include".to_string(), "catalog".to_string()),
            ("extend".to_string(), "extendedAssetUrls".to_string()),
            ("l".to_string(), self.settings.language.clone()),
        ];
        self.get(&format!("/v1/me/library/songs/{}", encode(id)), &query)
            .await
    }

    // ------------------------------------------------------------------
    // Playback assets, HLS manifests and license exchange
    // ------------------------------------------------------------------

    /// Catalog playback asset discovery. The response carries HLS URLs and
    /// license hints, never a content key.
    pub async fn get_play_assets(&self, catalog_id: &str) -> Result<Value, Failure> {
        let query = vec![
            ("id".to_string(), catalog_id.to_string()),
            ("kind".to_string(), "song".to_string()),
            ("includeLicenseUrls".to_string(), "true".to_string()),
            ("hlsEncryption".to_string(), "CBC".to_string()),
            ("hlsProfile".to_string(), "enhancedHls".to_string()),
        ];
        self.get("/v1/play/assets", &query).await
    }

    /// Fetch an HLS playlist as text. CDN requests must not inherit Apple API
    /// authorization cookies, so this deliberately skips [`Self::headers`].
    pub async fn fetch_text(&self, url: &str) -> Result<String, Failure> {
        self.check_cancelled()?;
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(invalid_response("Playback asset URL is not absolute"));
        }
        let response = self.http.get(url).send().await.map_err(|error| {
            if error.is_timeout() {
                timeout("Playback asset request timed out")
            } else {
                network("Playback asset request failed")
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(invalid_response(format!(
                "Playback asset returned HTTP {}",
                status.as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PLAYLIST_BYTES as u64)
        {
            return Err(invalid_response("Playback asset is not a playlist"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| network("Playback asset body was interrupted"))?;
        if bytes.len() > MAX_PLAYLIST_BYTES {
            return Err(invalid_response("Playback asset is not a playlist"));
        }
        String::from_utf8(bytes.to_vec())
            .map_err(|_| invalid_response("Playback asset is not UTF-8 text"))
    }

    /// Exchange a CDM challenge for a license. The challenge and the returned
    /// license are opaque binary blobs and are never logged or cached here.
    pub async fn license_exchange(
        &self,
        catalog_id: &str,
        key_uri: &str,
        challenge: &[u8],
        key_system: &str,
    ) -> Result<Vec<u8>, Failure> {
        self.require_login()?;
        let body = serde_json::json!({
            "challenge": base64::engine::general_purpose::STANDARD.encode(challenge),
            "key-system": key_system,
            "uri": key_uri,
            "adamId": catalog_id,
            "isLibrary": false,
            "user-initiated": true,
        });
        let value = self
            .send(Method::POST, LICENSE_URL.to_string(), &[], Some(body))
            .await?;
        if value.get("status").and_then(Value::as_i64) != Some(0) {
            return Err(authentication("Apple Music rejected the license request"));
        }
        let encoded = value
            .get("license")
            .and_then(Value::as_str)
            .filter(|license| !license.is_empty())
            .ok_or_else(|| invalid_response("Apple Music returned no license"))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| invalid_response("Apple Music license was not valid base64"))
    }

    fn require_login(&self) -> Result<(), Failure> {
        if self.is_logged_in() {
            Ok(())
        } else {
            Err(authentication(
                "Apple Music media user token is required for library access",
            ))
        }
    }
}

fn clamp_page(limit: i32) -> i32 {
    limit.clamp(1, MAX_PAGE)
}

fn encode(id: &str) -> String {
    // Apple IDs are opaque; encode anything that is not unreserved.
    let mut out = String::with_capacity(id.len());
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_page_bounds() {
        assert_eq!(clamp_page(0), 1);
        assert_eq!(clamp_page(-3), 1);
        assert_eq!(clamp_page(30), 30);
        assert_eq!(clamp_page(5_000), MAX_PAGE);
    }

    #[test]
    fn encodes_opaque_ids() {
        assert_eq!(encode("1440857781"), "1440857781");
        assert_eq!(encode("a/b?c"), "a%2Fb%3Fc");
    }
}
