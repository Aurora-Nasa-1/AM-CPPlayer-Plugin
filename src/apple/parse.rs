//! Mapping from Apple Music JSON resources to SDK records.

use serde_json::Value;

use crate::error::invalid_response;
use crate::models::Failure;
use crate::models::{
    Album, AlbumDetails, AlbumPage, Artist, ArtistPage, CatalogArtist, Lyrics, Playlist,
    PlaylistPage, Track, TrackPage,
};

/// Apple artwork templates expand to a concrete size; keep one variant.
pub fn cover_url(artwork: &Value, size: &str) -> Option<String> {
    let raw = artwork.get("url")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let size = if size.is_empty() { "1200" } else { size };
    let substituted = if raw.contains("{w}") || raw.contains("{h}") {
        raw.replace("{w}", size)
            .replace("{h}", size)
            .replace("{f}", "jpg")
    } else {
        raw.to_string()
    };
    Some(substituted)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn number(value: &Value, key: &str) -> Option<i64> {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(crate) fn first_data(payload: &Value) -> Result<&Value, Failure> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| invalid_response("Apple Music returned no resource data"))
}

fn scoped_id(id: &str, library: bool) -> String {
    if library && !id.starts_with("library/") {
        format!("library/{id}")
    } else {
        id.to_string()
    }
}

fn artists(attributes: &Value) -> Vec<Artist> {
    let mut result: Vec<Artist> = Vec::new();
    // Structured credits carry IDs; prefer them over the flat name list.
    for entry in array(attributes, "artists") {
        let name = entry
            .get("attributes")
            .and_then(|attributes| attributes.get("name"))
            .and_then(Value::as_str)
            .or_else(|| entry.get("name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let Some(name) = name else { continue };
        result.push(Artist {
            id: entry.get("id").and_then(Value::as_str).map(str::to_string),
            name: name.to_string(),
        });
    }
    if !result.is_empty() {
        return result;
    }
    for artist in array(attributes, "artistNames")
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        result.push(Artist {
            id: None,
            name: artist.to_string(),
        });
    }
    if result.is_empty() {
        // Search results only carry the flat `artistName` field unless the
        // caller requested the `artists` relationship.
        if let Some(name) = text(attributes, "artistName") {
            result.push(Artist { id: None, name });
        }
    }
    result
}

/// Map one catalog/library song resource to a [`Track`].
///
/// `library` prefixes the returned ID with `library/` so a later `resolve`
/// routes to the library playback endpoint. The contract requires `get` to
/// return the same provider-local ID it was given.
pub fn track(resource: &Value, cover_size: &str, library: bool) -> Result<Track, Failure> {
    let id = resource
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| invalid_response("Apple Music song has no ID"))?;
    let attributes = resource.get("attributes").unwrap_or(&Value::Null);
    let title = text(attributes, "name").unwrap_or_else(|| id.to_string());
    let duration_ms = number(attributes, "durationInMillis").unwrap_or(0);
    let album = text(attributes, "albumName");
    let cover_url = attributes
        .get("artwork")
        .and_then(|artwork| cover_url(artwork, cover_size));
    Ok(Track {
        id: scoped_id(id, library),
        title,
        artists: artists(attributes),
        durationMs: duration_ms,
        album,
        coverUrl: cover_url,
    })
}

/// Map one catalog/library album resource to an [`Album`].
pub fn album(resource: &Value, cover_size: &str) -> Result<Album, Failure> {
    let id = resource
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| invalid_response("Apple Music album has no ID"))?;
    let attributes = resource.get("attributes").unwrap_or(&Value::Null);
    Ok(Album {
        id: id.to_string(),
        title: text(attributes, "name").unwrap_or_else(|| id.to_string()),
        artists: artists(attributes),
        coverUrl: attributes
            .get("artwork")
            .and_then(|artwork| cover_url(artwork, cover_size)),
        trackCount: number(attributes, "trackCount")
            .map(|count| count.clamp(0, i32::MAX as i64) as i32),
        releasedAtEpochMs: None,
        company: text(attributes, "recordLabel"),
        description: text(attributes, "editorialNotes").or_else(|| {
            attributes
                .get("editorialNotes")
                .and_then(|notes| notes.get("standard"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
    })
}

/// Map one catalog/library playlist resource to a [`Playlist`].
pub fn playlist(resource: &Value, cover_size: &str) -> Result<Playlist, Failure> {
    let id = resource
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| invalid_response("Apple Music playlist has no ID"))?;
    let attributes = resource.get("attributes").unwrap_or(&Value::Null);
    Ok(Playlist {
        id: id.to_string(),
        title: text(attributes, "name").unwrap_or_else(|| id.to_string()),
        coverUrl: attributes
            .get("artwork")
            .and_then(|artwork| cover_url(artwork, cover_size)),
        trackCount: number(attributes, "trackCount")
            .map(|count| count.clamp(0, i32::MAX as i64) as i32),
        creatorId: None,
        creatorName: text(attributes, "curatorName"),
        description: text(attributes, "description").or_else(|| {
            attributes
                .get("description")
                .and_then(|description| description.get("standard"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
    })
}

/// Map one catalog artist resource to a [`CatalogArtist`].
pub fn catalog_artist(resource: &Value, cover_size: &str) -> Result<CatalogArtist, Failure> {
    let id = resource
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| invalid_response("Apple Music artist has no ID"))?;
    let attributes = resource.get("attributes").unwrap_or(&Value::Null);
    Ok(CatalogArtist {
        id: id.to_string(),
        name: text(attributes, "name").unwrap_or_else(|| id.to_string()),
        avatarUrl: attributes
            .get("artwork")
            .and_then(|artwork| cover_url(artwork, cover_size)),
        description: text(attributes, "editorialNotes").or_else(|| {
            attributes
                .get("editorialNotes")
                .and_then(|notes| notes.get("standard"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        aliases: Vec::new(),
        // Apple's artist resource carries no reliable counts; do not invent them.
        albumCount: None,
        trackCount: None,
    })
}

fn total(payload: &Value) -> Option<i64> {
    payload
        .get("meta")
        .and_then(|meta| meta.get("total"))
        .and_then(Value::as_i64)
}

fn has_next(payload: &Value) -> Option<bool> {
    payload.get("next").map(|next| !next.is_null())
}

/// Extract `data` into a vector of resources.
pub fn resources(payload: &Value) -> Result<Vec<&Value>, Failure> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .ok_or_else(|| invalid_response("Apple Music returned no data list"))
}

/// Page tracks, preserving order and honoring the requested item bound.
pub fn track_page(
    payload: &Value,
    limit: i32,
    cover_size: &str,
    library: bool,
) -> Result<TrackPage, Failure> {
    let bound = limit.max(0) as usize;
    let mut items = Vec::new();
    for resource in resources(payload)? {
        if items.len() >= bound {
            break;
        }
        // Playlists and albums may embed music videos; the SDK track page is
        // audio-only, so other types are skipped rather than mislabeled.
        if let Some(kind) = resource.get("type").and_then(Value::as_str) {
            if !kind.ends_with("songs") && kind != "song" && kind != "songs" {
                continue;
            }
        }
        match track(resource, cover_size, library) {
            Ok(track) => items.push(track),
            Err(_) => continue,
        }
    }
    Ok(TrackPage {
        items,
        total: total(payload),
        hasMore: has_next(payload),
    })
}

pub fn album_page(payload: &Value, limit: i32, cover_size: &str) -> Result<AlbumPage, Failure> {
    let bound = limit.max(0) as usize;
    let mut items = Vec::new();
    for resource in resources(payload)? {
        if items.len() >= bound {
            break;
        }
        if let Ok(album) = album(resource, cover_size) {
            items.push(album);
        }
    }
    Ok(AlbumPage {
        items,
        total: total(payload),
        hasMore: has_next(payload),
    })
}

pub fn playlist_page(
    payload: &Value,
    limit: i32,
    cover_size: &str,
) -> Result<PlaylistPage, Failure> {
    let bound = limit.max(0) as usize;
    let mut items = Vec::new();
    for resource in resources(payload)? {
        if items.len() >= bound {
            break;
        }
        if let Ok(playlist) = playlist(resource, cover_size) {
            items.push(playlist);
        }
    }
    Ok(PlaylistPage {
        items,
        total: total(payload),
        hasMore: has_next(payload),
    })
}

pub fn artist_page(payload: &Value, limit: i32, cover_size: &str) -> Result<ArtistPage, Failure> {
    let bound = limit.max(0) as usize;
    let mut items = Vec::new();
    for resource in resources(payload)? {
        if items.len() >= bound {
            break;
        }
        if let Ok(artist) = catalog_artist(resource, cover_size) {
            items.push(artist);
        }
    }
    Ok(ArtistPage {
        items,
        total: total(payload),
        hasMore: has_next(payload),
    })
}

/// Search responses group results under `results[type]`; take one group.
pub fn search_group<'a>(payload: &'a Value, group: &str) -> Result<&'a Value, Failure> {
    payload
        .get("results")
        .and_then(|results| results.get(group))
        .ok_or_else(|| invalid_response("Apple Music search returned no result group"))
}

/// Map a song resource directly, for `get`.
pub fn song_from_payload(payload: &Value, cover_size: &str) -> Result<Track, Failure> {
    track(first_data(payload)?, cover_size, false)
}

pub fn album_details(
    payload: &Value,
    cover_size: &str,
    library: bool,
) -> Result<AlbumDetails, Failure> {
    let resource = first_data(payload)?;
    let album = album(resource, cover_size)?;
    let mut tracks = Vec::new();
    if let Some(relationship) = resource.get("relationships").and_then(|r| r.get("tracks")) {
        for item in array(relationship, "data") {
            if tracks.len() >= 500 {
                break;
            }
            if let Ok(track) = track(item, cover_size, library) {
                tracks.push(track);
            }
        }
    }
    Ok(AlbumDetails { album, tracks })
}

pub fn playlist_from_payload(payload: &Value, cover_size: &str) -> Result<Playlist, Failure> {
    playlist(first_data(payload)?, cover_size)
}

pub fn artist_from_payload(payload: &Value, cover_size: &str) -> Result<CatalogArtist, Failure> {
    catalog_artist(first_data(payload)?, cover_size)
}

/// Map the Apple lyrics resource into the SDK lyrics record.
pub fn lyrics(payload: &Value, song_id: &str) -> Result<Lyrics, Failure> {
    let attributes = first_data(payload)
        .ok()
        .and_then(|resource| resource.get("attributes"))
        .cloned()
        .unwrap_or(Value::Null);
    let ttml = text(&attributes, "ttml");
    let lrc = text(&attributes, "lrc");
    if ttml.is_none() && lrc.is_none() {
        return Err(crate::error::not_found("No lyrics available for this song"));
    }
    Ok(Lyrics {
        id: song_id.to_string(),
        lrc,
        ttml,
        yrc: None,
        translation: None,
        romanization: None,
    })
}

/// Extract the numeric catalog song ID from a library song resource.
///
/// `include=catalog` puts the catalog song in `relationships.catalog`; older
/// payloads only expose the same identity through `attributes.playParams.id`.
pub fn catalog_id_from_library(payload: &Value) -> Option<String> {
    let resource = first_data(payload).ok()?;
    resource
        .pointer("/relationships/catalog/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(std::iter::once(resource))
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .chain(
            resource
                .pointer("/attributes/playParams/id")
                .and_then(Value::as_str),
        )
        .map(str::trim)
        .find(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn song() -> Value {
        json!({
            "id": "1440857781",
            "type": "songs",
            "attributes": {
                "name": "Never Gonna Give You Up",
                "artistName": "Rick Astley",
                "artistNames": ["Rick Astley"],
                "albumName": "Whenever You Need Somebody",
                "durationInMillis": 213573,
                "artwork": { "url": "https://is1-ssl.mzstatic.com/image/{w}x{h}bb.jpg" }
            }
        })
    }

    #[test]
    fn maps_song_metadata() {
        let track = track(&song(), "1200", false).unwrap();
        assert_eq!(track.id, "1440857781");
        assert_eq!(track.title, "Never Gonna Give You Up");
        assert_eq!(track.artists[0].name, "Rick Astley");
        assert_eq!(track.album.as_deref(), Some("Whenever You Need Somebody"));
        assert_eq!(track.durationMs, 213_573);
        assert_eq!(
            track.coverUrl.as_deref(),
            Some("https://is1-ssl.mzstatic.com/image/1200x1200bb.jpg")
        );
    }

    #[test]
    fn keeps_missing_optional_metadata_null() {
        let track = track(
            &json!({ "id": "1", "attributes": { "name": "x" } }),
            "600",
            false,
        )
        .unwrap();
        assert!(track.album.is_none());
        assert!(track.coverUrl.is_none());
        assert_eq!(track.durationMs, 0);
    }

    #[test]
    fn falls_back_to_flat_artist_name() {
        let resource = json!({
            "id": "1",
            "attributes": { "name": "x", "artistName": "Rick Astley" }
        });
        let mapped = track(&resource, "300", false).unwrap();
        assert_eq!(mapped.artists.len(), 1);
        assert_eq!(mapped.artists[0].name, "Rick Astley");
        assert!(mapped.artists[0].id.is_none());
    }

    #[test]
    fn pages_bound_items_and_skip_non_audio() {
        let payload = json!({
            "data": [
                song(),
                { "id": "mv1", "type": "music-videos", "attributes": { "name": "Video" } },
                song()
            ],
            "meta": { "total": 3 },
            "next": "/v1/next"
        });
        let page = track_page(&payload, 1, "300", false).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total, Some(3));
        assert_eq!(page.hasMore, Some(true));
    }

    #[test]
    fn requires_id_for_tracks() {
        assert!(track(&json!({ "attributes": { "name": "x" } }), "300", false).is_err());
    }

    #[test]
    fn library_tracks_keep_the_scoped_id() {
        let mapped = track(&song(), "300", true).unwrap();
        assert_eq!(mapped.id, "library/1440857781");
        // An already-scoped ID must not be prefixed twice.
        let again = track(
            &json!({ "id": "library/1", "attributes": { "name": "x" } }),
            "300",
            true,
        )
        .unwrap();
        assert_eq!(again.id, "library/1");
    }

    #[test]
    fn lyrics_requires_a_format() {
        let payload = json!({ "data": [{ "attributes": {} }] });
        assert!(lyrics(&payload, "1").is_err());
        let payload = json!({ "data": [{ "attributes": { "ttml": "<tt/>" } }] });
        let parsed = lyrics(&payload, "1").unwrap();
        assert_eq!(parsed.ttml.as_deref(), Some("<tt/>"));
        assert_eq!(parsed.id, "1");
    }

    #[test]
    fn maps_library_song_to_catalog_id() {
        let relationship = json!({
            "data": [{
                "id": "i.private",
                "relationships": {
                    "catalog": { "data": [{ "id": "269573303" }] }
                }
            }]
        });
        assert_eq!(
            catalog_id_from_library(&relationship).as_deref(),
            Some("269573303")
        );

        let play_params = json!({
            "data": [{
                "id": "i.private",
                "attributes": { "playParams": { "id": "1440857781" } }
            }]
        });
        assert_eq!(
            catalog_id_from_library(&play_params).as_deref(),
            Some("1440857781")
        );

        // An opaque library ID must never be sent as a catalog ID.
        let opaque = json!({ "data": [{ "id": "i.private" }] });
        assert!(catalog_id_from_library(&opaque).is_none());
    }
}
