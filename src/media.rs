//! Audio resolution.
//!
//! Apple subscription playback assets are FairPlay-protected HLS streams.
//! [`MediaResource`] has no DRM signaling or license channel, so handing those
//! URLs to the host would only fail inside the player. The public
//! `attributes.previews` AAC file is unencrypted and is the one asset this
//! provider can return. Everything else fails explicitly.

use serde_json::Value;

use crate::apple::{parse, AppleClient};
use crate::error::{not_found, unsupported};
use crate::models::{Failure, MediaResource};
use crate::settings::Settings;

/// `library/{id}` marks a personal-library resource.
const LIBRARY_PREFIX: &str = "library/";

pub fn is_library_id(id: &str) -> bool {
    id.starts_with(LIBRARY_PREFIX)
}

pub fn resource_id(id: &str) -> &str {
    id.strip_prefix(LIBRARY_PREFIX).unwrap_or(id)
}

pub async fn resolve(
    client: &AppleClient,
    id: &str,
    quality: &str,
    settings: &Settings,
) -> Result<MediaResource, Failure> {
    let preview_requested = is_preview_quality(quality);
    if is_library_id(id) {
        return resolve_library(client, resource_id(id), preview_requested, settings).await;
    }
    if !preview_requested && !settings.allow_preview {
        return Err(unsupported(
            "Apple Music streams are DRM-protected and previews are disabled",
        ));
    }
    resolve_catalog_preview(client, id).await
}

fn is_preview_quality(quality: &str) -> bool {
    matches!(
        quality.trim().to_ascii_lowercase().as_str(),
        "preview" | "sample"
    )
}

/// Library IDs are opaque (`i.*`); map one to its catalog counterpart before
/// asking for the public preview of that catalog song.
async fn resolve_library(
    client: &AppleClient,
    id: &str,
    preview_requested: bool,
    settings: &Settings,
) -> Result<MediaResource, Failure> {
    let payload = client.get_library_song(id).await?;
    if !preview_requested && !settings.allow_preview {
        return Err(unsupported(
            "Apple Music streams are DRM-protected and previews are disabled",
        ));
    }
    let catalog_id = parse::catalog_id_from_library(&payload)
        .ok_or_else(|| unsupported("Library song has no catalog counterpart for playback"))?;
    resolve_catalog_preview(client, &catalog_id).await
}

/// The song resource's public, unencrypted `previews` entries.
async fn resolve_catalog_preview(client: &AppleClient, id: &str) -> Result<MediaResource, Failure> {
    let payload = client.get_song(id).await?;
    let url = preview_url(&payload)
        .ok_or_else(|| not_found("Apple Music has no playable preview for this track"))?;
    Ok(MediaResource {
        url: url.to_string(),
        actualQuality: "preview".to_string(),
        headers: Vec::new(),
        expiresAtEpochMs: None,
        format: Some("m4a".to_string()),
        bitrate: None,
        sizeBytes: None,
    })
}

fn preview_url(payload: &Value) -> Option<&str> {
    let attributes = parse::first_data(payload).ok()?.get("attributes")?;
    attributes
        .get("previews")?
        .as_array()?
        .iter()
        .filter_map(|preview| preview.get("url").and_then(Value::as_str))
        .find(|url| is_https(url))
}

fn is_https(url: &str) -> bool {
    url.starts_with("https://") && url.len() > "https://".len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn marks_library_ids() {
        assert!(is_library_id("library/i.abc"));
        assert_eq!(resource_id("library/i.abc"), "i.abc");
        assert_eq!(resource_id("1440857781"), "1440857781");
    }

    #[test]
    fn picks_first_https_preview() {
        let payload = json!({
            "data": [{
                "attributes": {
                    "previews": [
                        { "url": "http://insecure/preview.m4a" },
                        { "url": "https://audio-ssl.example/preview.m4a" },
                        { "url": "https://audio-ssl.example/second.m4a" }
                    ]
                }
            }]
        });
        assert_eq!(
            preview_url(&payload),
            Some("https://audio-ssl.example/preview.m4a")
        );
    }

    #[test]
    fn rejects_payload_without_preview() {
        assert!(preview_url(&json!({ "data": [{ "attributes": {} }] })).is_none());
        assert!(preview_url(&json!({ "data": [] })).is_none());
    }

    #[test]
    fn recognizes_explicit_preview_quality() {
        assert!(is_preview_quality("preview"));
        assert!(is_preview_quality(" Sample "));
        assert!(!is_preview_quality("standard"));
        assert!(!is_preview_quality(""));
    }
}
