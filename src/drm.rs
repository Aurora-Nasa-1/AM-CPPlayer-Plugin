//! HLS DRM signaling parsing for Apple Music playback assets.
//!
//! This module deliberately stops at signaling. It never contains a content
//! key, a reusable license, or a CDM. The host CDM supplies the challenge and
//! receives the provider license through `exchangeLicense`.

use std::collections::HashSet;

use base64::Engine;
use serde_json::Value;
use url::Url;

use crate::apple::{parse, AppleClient};
use crate::error::{invalid_response, not_found, unsupported};
use crate::media;
use crate::models::{
    DrmDescriptor, DrmResolveRequest, Failure, Header, LicenseRequest, LicenseResponse,
    MediaResource, ResolvedMedia,
};

const MAX_ID_BYTES: usize = 1024;
const MAX_QUALITY_BYTES: usize = 128;
const MAX_KEYSYSTEMS: usize = 8;
const MAX_CHALLENGE_BYTES: usize = 1024 * 1024;

pub const WIDEVINE_SCHEME: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
pub const PLAYREADY_SCHEME: &str = "com.microsoft.playready";
pub const FAIRPLAY_SCHEME: &str = "com.apple.streamingkeydelivery";

#[derive(Debug, Clone, PartialEq)]
pub struct HlsKey {
    pub key_system: Option<String>,
    pub scheme_id_uri: Option<String>,
    pub init_data: Option<Vec<u8>>,
    pub key_id: Option<Vec<u8>>,
    pub key_uri: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: u64,
    pub audio: Option<String>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ManifestInfo {
    pub keys: Vec<HlsKey>,
    pub variants: Vec<Variant>,
}

/// Canonicalize the host identifiers and the Apple/HLS aliases.
pub fn canonical_key_system(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "widevine" | "com.widevine.alpha" | WIDEVINE_SCHEME => Some("widevine"),
        "playready" | PLAYREADY_SCHEME => Some("playready"),
        "fairplay" | FAIRPLAY_SCHEME => Some("fairplay"),
        "clearkey" | "org.w3.clearkey" => Some("clearkey"),
        _ => None,
    }
}

pub fn apple_key_system(canonical: &str) -> Option<&'static str> {
    match canonical {
        "widevine" => Some("com.widevine.alpha"),
        "playready" => Some("com.microsoft.playready"),
        "fairplay" => Some("com.apple.streamingkeydelivery"),
        _ => None,
    }
}

pub fn scheme_for(canonical: &str) -> Option<&'static str> {
    match canonical {
        "widevine" => Some(WIDEVINE_SCHEME),
        "playready" => Some(PLAYREADY_SCHEME),
        "fairplay" => Some(FAIRPLAY_SCHEME),
        "clearkey" => Some("org.w3.clearkey"),
        _ => None,
    }
}

pub fn is_lossless_quality(quality: &str) -> bool {
    let quality = quality.trim().to_ascii_lowercase();
    quality.contains("lossless")
        || quality.contains("alac")
        || quality.contains("hires")
        || quality.contains("hi-res")
}

pub fn actual_quality(quality: &str) -> &'static str {
    if is_lossless_quality(quality) {
        "lossless"
    } else {
        "standard"
    }
}

/// Parse an HLS master or media playlist. Relative variant/key URIs are
/// resolved against the playlist URL so a later request never has to guess.
pub fn parse_manifest(text: &str, base_url: &str) -> ManifestInfo {
    let mut info = ManifestInfo::default();
    let mut pending: Option<Vec<(String, String)>> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
            let attributes = line.split_once(':').map(|(_, rest)| rest).unwrap_or("");
            if let Some(key) = parse_key(attributes, base_url) {
                info.keys.push(key);
            }
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF:") {
            let attributes = line.split_once(':').map(|(_, rest)| rest).unwrap_or("");
            pending = Some(parse_attributes(attributes));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(attributes) = pending.take() {
            if let Some(uri) = resolve_url(base_url, line) {
                info.variants.push(Variant {
                    uri,
                    bandwidth: attribute(&attributes, "BANDWIDTH")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    audio: attribute(&attributes, "AUDIO"),
                    codecs: attribute(&attributes, "CODECS"),
                });
            }
        }
    }
    info
}

fn parse_key(attributes: &str, base_url: &str) -> Option<HlsKey> {
    let values = parse_attributes(attributes);
    let method = attribute(&values, "METHOD").unwrap_or_else(|| "NONE".to_string());
    if method.eq_ignore_ascii_case("NONE") {
        return None;
    }
    let raw_uri = attribute(&values, "URI")?;
    let key_uri = if raw_uri.starts_with("data:") || raw_uri.starts_with("skd://") {
        raw_uri
    } else {
        resolve_url(base_url, &raw_uri)?
    };
    let key_format = attribute(&values, "KEYFORMAT").filter(|value| !value.is_empty());
    let key_system = key_format
        .as_deref()
        .and_then(canonical_key_system)
        .map(str::to_string);
    let scheme_id_uri = key_format.clone().or_else(|| {
        key_system
            .as_deref()
            .and_then(scheme_for)
            .map(str::to_string)
    });
    Some(HlsKey {
        key_system,
        scheme_id_uri,
        init_data: decode_data_uri(&key_uri),
        key_id: attribute(&values, "KEYID").and_then(|value| parse_key_id(&value)),
        key_uri,
        method,
    })
}

fn parse_attributes(input: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b',' || bytes[index].is_ascii_whitespace()) {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' && bytes[index] != b',' {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            break;
        }
        let key = input[key_start..index].trim();
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let value = if index < bytes.len() && bytes[index] == b'"' {
            index += 1;
            let value_start = index;
            while index < bytes.len() && bytes[index] != b'"' {
                index += 1;
            }
            let value = input[value_start..index].to_string();
            if index < bytes.len() {
                index += 1;
            }
            value
        } else {
            let value_start = index;
            while index < bytes.len() && bytes[index] != b',' {
                index += 1;
            }
            input[value_start..index].trim().to_string()
        };
        if !key.is_empty() {
            result.push((key.to_string(), value));
        }
    }
    result
}

fn attribute(attributes: &[(String, String)], name: &str) -> Option<String> {
    attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn resolve_url(base_url: &str, value: &str) -> Option<String> {
    if value.starts_with("https://") || value.starts_with("http://") {
        return Some(value.to_string());
    }
    Url::parse(base_url)
        .ok()?
        .join(value)
        .ok()
        .map(|url| url.to_string())
}

fn decode_data_uri(value: &str) -> Option<Vec<u8>> {
    let (_, payload) = value.split_once(";base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()
}

fn parse_key_id(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        if hex.len() % 2 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(hex.len() / 2);
        for pair in hex.as_bytes().chunks_exact(2) {
            let text = std::str::from_utf8(pair).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
        }
        return (!out.is_empty()).then_some(out);
    }
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

/// Select the highest-priority requested key system that the manifest can
/// actually serve. Untyped `ISO-23001-7` PSSH is only used for Widevine or
/// PlayReady, never silently reported as FairPlay.
pub fn select_key<'a>(
    keys: &'a [HlsKey],
    requested: &[String],
) -> Option<(&'a HlsKey, &'static str)> {
    for requested_system in requested {
        let Some(canonical) = canonical_key_system(requested_system) else {
            continue;
        };
        if canonical == "clearkey" {
            continue;
        }
        if let Some(key) = keys
            .iter()
            .find(|key| key.key_system.as_deref() == Some(canonical))
        {
            return Some((key, canonical));
        }
        if matches!(canonical, "widevine" | "playready") {
            if let Some(key) = keys.iter().find(|key| {
                key.key_system.is_none()
                    && key.init_data.is_some()
                    && key.method.eq_ignore_ascii_case("ISO-23001-7")
            }) {
                return Some((key, canonical));
            }
        }
    }
    None
}

pub fn descriptor(key: &HlsKey, key_system: &str) -> DrmDescriptor {
    DrmDescriptor {
        keySystem: key_system.to_string(),
        licenseUrl: None,
        headers: Vec::<Header>::new(),
        schemeIdUri: key
            .scheme_id_uri
            .clone()
            .or_else(|| scheme_for(key_system).map(str::to_string)),
        initData: key.init_data.clone(),
        keyId: key.key_id.clone(),
        keyUri: Some(key.key_uri.clone()).filter(|uri| !uri.is_empty()),
        serverCertificate: None,
        persistent: false,
    }
}

pub fn select_variant<'a>(info: &'a ManifestInfo, quality: &str) -> Option<&'a Variant> {
    if info.variants.is_empty() {
        return None;
    }
    let lossless = is_lossless_quality(quality);
    let mut candidates: Vec<&Variant> = info
        .variants
        .iter()
        .filter(|variant| {
            let group = variant.audio.as_deref().unwrap_or("").to_ascii_lowercase();
            let codecs = variant.codecs.as_deref().unwrap_or("").to_ascii_lowercase();
            let is_alac = group.contains("alac") || codecs.contains("alac");
            if lossless {
                is_alac
            } else {
                !is_alac
            }
        })
        .collect();
    if candidates.is_empty() {
        candidates = info.variants.iter().collect();
    }
    candidates
        .into_iter()
        .max_by_key(|variant| variant.bandwidth)
}

/// Pull the first HTTPS playback asset from `/v1/play/assets`.
pub fn first_asset_url(payload: &Value) -> Option<&str> {
    let assets = payload
        .pointer("/results/assets")
        .and_then(Value::as_array)
        .or_else(|| payload.get("assets").and_then(Value::as_array))?;
    assets.iter().find_map(|asset| {
        asset
            .get("url")
            .or_else(|| asset.get("URL"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| url.starts_with("https://") && url.len() > "https://".len())
    })
}

/// Apple signs a concrete variant file name; the durable enhanced-HLS master
/// is the `_default` sibling (gamdl 3.x applies the same normalization).
fn default_master_url(value: &str) -> String {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex_lite::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex_lite::Regex::new(r"(P\d+)_[^/?]+(\.m3u8)").expect("valid HLS master pattern")
    });
    pattern.replace_all(value, "${1}_default${2}").into_owned()
}

/// Resolve a track to an encrypted (or, when the manifest is clear, plain)
/// playback resource using only signaling supplied by Apple's play assets.
pub async fn resolve(
    client: &AppleClient,
    request: &DrmResolveRequest,
    _settings: &crate::settings::Settings,
) -> Result<ResolvedMedia, Failure> {
    let id = request.id.trim();
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err(not_found("Invalid Apple Music resource ID"));
    }
    let quality = request.quality.trim();
    if quality.is_empty() || quality.len() > MAX_QUALITY_BYTES {
        return Err(invalid_response("Invalid DRM quality selector"));
    }
    let key_systems = canonical_key_systems(&request.keySystems)?;

    let catalog_id = catalog_id(client, id).await?;
    let assets = client.get_play_assets(&catalog_id).await?;
    let master_url = default_master_url(
        first_asset_url(&assets)
            .ok_or_else(|| not_found("Apple Music returned no playable asset"))?,
    );
    let master_text = client.fetch_text(&master_url).await?;
    let mut manifest = parse_manifest(&master_text, &master_url);

    let variant = select_variant(&manifest, quality).cloned();
    let (resource_url, actual_quality, bitrate) = match variant {
        Some(variant) => {
            // Media playlists may carry the actual EXT-X-KEY; the master often
            // only advertises variants. Merge both before selecting a key.
            let media_text = client.fetch_text(&variant.uri).await?;
            let media = parse_manifest(&media_text, &variant.uri);
            manifest.keys.extend(media.keys);
            let quality = if is_alac_variant(&variant) {
                "lossless"
            } else {
                "standard"
            };
            (
                // Play the selected media playlist. Returning the master would
                // let the host pick another rendition, making actualQuality a
                // statement about the manifest rather than the played stream.
                variant.uri.clone(),
                quality,
                i32::try_from(variant.bandwidth)
                    .ok()
                    .filter(|value| *value > 0),
            )
        }
        None => (master_url.clone(), actual_quality(quality), None),
    };

    let drm = if manifest.keys.is_empty() {
        None
    } else {
        let (key, key_system) = select_key(&manifest.keys, &key_systems).ok_or_else(|| {
            unsupported("No requested DRM key system is available for this asset")
        })?;
        Some(descriptor(key, key_system))
    };

    Ok(ResolvedMedia {
        resource: MediaResource {
            url: resource_url,
            actualQuality: actual_quality.to_string(),
            headers: Vec::new(),
            expiresAtEpochMs: None,
            format: Some("hls".to_string()),
            bitrate,
            sizeBytes: None,
        },
        drm,
    })
}

/// Exchange a host CDM challenge with Apple's web playback license endpoint.
pub async fn exchange_license(
    client: &AppleClient,
    request: LicenseRequest,
) -> Result<LicenseResponse, Failure> {
    let id = request.id.trim();
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err(not_found("Invalid Apple Music resource ID"));
    }
    let canonical = canonical_key_system(&request.keySystem)
        .ok_or_else(|| invalid_response("Unsupported DRM key system"))?;
    let apple_system = apple_key_system(canonical)
        .ok_or_else(|| unsupported("Apple Music cannot issue licenses for this key system"))?;
    if request.challenge.is_empty() || request.challenge.len() > MAX_CHALLENGE_BYTES {
        return Err(invalid_response("Invalid DRM license challenge"));
    }
    let key_uri = request
        .keyUri
        .as_deref()
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| invalid_response("DRM license exchange requires a key URI"))?;
    // Apple authenticates the exchange with the media user token; caller
    // supplied headers are hints for other hosts, not replacements here.
    let _ = request.headers;
    let _ = request.licenseUrl;
    let catalog_id = catalog_id(client, id).await?;
    let license = client
        .license_exchange(&catalog_id, key_uri, &request.challenge, apple_system)
        .await?;
    Ok(LicenseResponse { license })
}

async fn catalog_id(client: &AppleClient, id: &str) -> Result<String, Failure> {
    if !media::is_library_id(id) {
        return Ok(id.to_string());
    }
    let payload = client.get_library_song(media::resource_id(id)).await?;
    parse::catalog_id_from_library(&payload)
        .ok_or_else(|| not_found("Library song has no catalog counterpart"))
}

fn canonical_key_systems(requested: &[String]) -> Result<Vec<String>, Failure> {
    if requested.is_empty() || requested.len() > MAX_KEYSYSTEMS {
        return Err(invalid_response("Invalid DRM key system list"));
    }
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(requested.len());
    for value in requested {
        let canonical = canonical_key_system(value)
            .ok_or_else(|| invalid_response("Unsupported DRM key system"))?;
        if !seen.insert(canonical) {
            return Err(invalid_response("Duplicate DRM key system"));
        }
        result.push(canonical.to_string());
    }
    Ok(result)
}

fn is_alac_variant(variant: &Variant) -> bool {
    let group = variant.audio.as_deref().unwrap_or("").to_ascii_lowercase();
    let codecs = variant.codecs.as_deref().unwrap_or("").to_ascii_lowercase();
    group.contains("alac") || codecs.contains("alac")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_widevine_key_and_variants() {
        let playlist = r#"
#EXTM3U
#EXT-X-SESSION-KEY:METHOD=ISO-23001-7,URI="data:text/plain;base64,cHNzaA==",KEYFORMAT="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",KEYID=0x00112233445566778899aabbccddeeff
#EXT-X-STREAM-INF:BANDWIDTH=256000,AUDIO="audio-aac",CODECS="mp4a.40.2"
audio/aac.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1411200,AUDIO="audio-alac-stereo-44-16",CODECS="alac"
audio/alac.m3u8
"#;
        let info = parse_manifest(playlist, "https://cdn.example.test/master.m3u8");
        assert_eq!(info.keys.len(), 1);
        assert_eq!(info.keys[0].key_system.as_deref(), Some("widevine"));
        assert_eq!(info.keys[0].init_data.as_deref(), Some(b"pssh".as_slice()));
        assert_eq!(info.keys[0].key_id.as_ref().map(Vec::len), Some(16));
        assert_eq!(info.variants.len(), 2);
        assert_eq!(
            select_variant(&info, "drm-lossless").map(|variant| variant.uri.as_str()),
            Some("https://cdn.example.test/audio/alac.m3u8")
        );
        assert_eq!(
            select_variant(&info, "drm-standard").map(|variant| variant.uri.as_str()),
            Some("https://cdn.example.test/audio/aac.m3u8")
        );
    }

    #[test]
    fn selects_requested_key_system_in_priority_order() {
        let keys = vec![
            HlsKey {
                key_system: Some("fairplay".to_string()),
                scheme_id_uri: Some(FAIRPLAY_SCHEME.to_string()),
                init_data: None,
                key_id: None,
                key_uri: "skd://itunes.example/track".to_string(),
                method: "SAMPLE-AES".to_string(),
            },
            HlsKey {
                key_system: Some("widevine".to_string()),
                scheme_id_uri: Some(WIDEVINE_SCHEME.to_string()),
                init_data: Some(vec![1, 2, 3]),
                key_id: None,
                key_uri: "data:text/plain;base64,AQID".to_string(),
                method: "ISO-23001-7".to_string(),
            },
        ];
        let requested = vec!["fairplay".to_string(), "widevine".to_string()];
        let (key, system) = select_key(&keys, &requested).expect("a key");
        assert_eq!(system, "fairplay");
        assert_eq!(key.key_uri, "skd://itunes.example/track");
    }

    #[test]
    fn untyped_pssh_is_not_reported_as_fairplay() {
        let keys = vec![HlsKey {
            key_system: None,
            scheme_id_uri: None,
            init_data: Some(vec![1, 2, 3]),
            key_id: None,
            key_uri: "data:text/plain;base64,AQID".to_string(),
            method: "ISO-23001-7".to_string(),
        }];
        assert!(select_key(&keys, &["fairplay".to_string()]).is_none());
        assert_eq!(
            select_key(&keys, &["widevine".to_string()]).map(|(_, system)| system),
            Some("widevine")
        );
    }

    #[test]
    fn rejects_invalid_key_system_aliases() {
        assert_eq!(canonical_key_system("com.widevine.alpha"), Some("widevine"));
        assert_eq!(canonical_key_system("fairplay"), Some("fairplay"));
        assert!(canonical_key_system("not-a-key-system").is_none());
    }

    #[test]
    fn normalizes_concrete_master_variants() {
        assert_eq!(
            default_master_url("https://cdn.example/abc/P42_123456.m3u8?token=x"),
            "https://cdn.example/abc/P42_default.m3u8?token=x"
        );
        assert_eq!(
            default_master_url("https://cdn.example/abc/master.m3u8"),
            "https://cdn.example/abc/master.m3u8"
        );
    }
}
