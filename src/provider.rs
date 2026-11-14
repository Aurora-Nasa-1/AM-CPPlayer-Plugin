//! The `kmp-sdk-v1` provider implementation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::apple::{parse, AppleClient};
use crate::error::{invalid_response, not_found, unsupported};
use crate::models::{
    AccountSession, AlbumDetails, AlbumPage, BatchRequest, Capabilities, DrmResolveRequest,
    Failure, IdRequest, LicenseRequest, LicenseResponse, Lyrics, PagedIdRequest, Playlist,
    PlaylistPage, ResolveRequest, ResolvedMedia, SearchRequest, Track, TrackBatch, TrackOutcome,
    TrackPage,
};
use crate::operations::{capabilities, tags, SUPPORTED};
use crate::settings::{Credential, Settings};
use crate::wire::{WireReader, WireWriter};
use crate::{drm, media};

pub struct AppleProvider {
    client: AppleClient,
    cancelled: Arc<AtomicBool>,
    settings: Settings,
}

impl AppleProvider {
    pub fn new(
        provider_id: String,
        account_id: String,
        credential: Option<String>,
        settings_json: Option<String>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, Failure> {
        let _ = (provider_id, account_id);
        let mut settings = Settings::from_json(settings_json.as_deref());
        let credential = Credential::parse(credential.as_deref());
        if let Some(storefront) = credential.storefront.as_deref() {
            settings.storefront = storefront.to_string();
        }
        if let Some(language) = credential.language.as_deref() {
            settings.language = language.to_string();
        }
        let developer_token = credential
            .developer_token
            .clone()
            .or_else(|| settings.developer_token.clone());
        let client = AppleClient::new(
            settings.clone(),
            credential.media_user_token,
            developer_token,
            cancelled.clone(),
        )?;
        Ok(Self {
            client,
            cancelled,
            settings,
        })
    }

    fn cover(&self) -> &str {
        &self.settings.cover_size
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Decode a request, dispatch, and encode the response payload.
    pub async fn dispatch(&self, operation: i32, input: &[u8]) -> Result<Vec<u8>, Failure> {
        let mut reader = WireReader::new(input)?;
        let payload = match operation {
            tags::CAPABILITIES => {
                skip_no_argument(&mut reader)?;
                encode(|w| {
                    Capabilities {
                        supported: SUPPORTED.iter().map(|item| item.to_string()).collect(),
                    }
                    .write(w)
                })
            }
            tags::SEARCH => {
                let request = SearchRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.search(request).await?;
                encode(|w| page.write(w))
            }
            tags::GET => {
                let request = IdRequest::read(&mut reader)?;
                reader.finish()?;
                let track = self.get(request).await?;
                encode(|w| track.write(w))
            }
            tags::GETMANY => {
                let request = BatchRequest::read(&mut reader)?;
                reader.finish()?;
                let batch = self.get_many(request).await?;
                encode(|w| batch.write(w))
            }
            tags::RESOLVE => {
                let request = ResolveRequest::read(&mut reader)?;
                reader.finish()?;
                let resource = self.resolve(request).await?;
                encode(|w| resource.write(w))
            }
            tags::RESOLVEDRM => {
                let request = DrmResolveRequest::read(&mut reader)?;
                reader.finish()?;
                let media = self.resolve_drm(request).await?;
                encode(|w| media.write(w))
            }
            tags::EXCHANGELICENSE => {
                let request = LicenseRequest::read(&mut reader)?;
                reader.finish()?;
                let response = self.exchange_license(request).await?;
                encode(|w| response.write(w))
            }
            tags::GETLYRICS => {
                let request = IdRequest::read(&mut reader)?;
                reader.finish()?;
                let lyrics = self.lyrics(request).await?;
                encode(|w| lyrics.write(w))
            }
            tags::SEARCHPLAYLISTS => {
                let request = SearchRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.search_playlists(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETPLAYLIST => {
                let request = IdRequest::read(&mut reader)?;
                reader.finish()?;
                let playlist = self.playlist(request).await?;
                encode(|w| playlist.write(w))
            }
            tags::GETPLAYLISTTRACKS => {
                let request = PagedIdRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.playlist_tracks(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETUSERPLAYLISTS => {
                let request = PagedIdRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.user_playlists(request).await?;
                encode(|w| page.write(w))
            }
            tags::SEARCHALBUMS => {
                let request = SearchRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.search_albums(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETALBUM => {
                let request = IdRequest::read(&mut reader)?;
                reader.finish()?;
                let details = self.album(request).await?;
                encode(|w| details.write(w))
            }
            tags::SEARCHARTISTS => {
                let request = SearchRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.search_artists(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETARTIST => {
                let request = IdRequest::read(&mut reader)?;
                reader.finish()?;
                let artist = self.artist(request).await?;
                encode(|w| artist.write(w))
            }
            tags::GETARTISTTRACKS => {
                let request = PagedIdRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.artist_tracks(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETARTISTALBUMS => {
                let request = PagedIdRequest::read(&mut reader)?;
                reader.finish()?;
                let page = self.artist_albums(request).await?;
                encode(|w| page.write(w))
            }
            tags::GETCURRENTACCOUNT => {
                skip_no_argument(&mut reader)?;
                let account = self.account().await?;
                encode(|w| account.write(w))
            }
            _ => return Err(unsupported("Operation is not supported by this provider")),
        }?;
        Ok(payload)
    }

    // ------------------------------------------------------------------
    // Operations
    // ------------------------------------------------------------------

    async fn search(&self, request: SearchRequest) -> Result<TrackPage, Failure> {
        let keyword = request.keyword.trim();
        if keyword.is_empty() {
            return Err(invalid_response("Search keyword must not be empty"));
        }
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .search(keyword, "songs", limit, request.offset.max(0))
            .await?;
        let group = parse::search_group(&payload, "songs")?;
        parse::track_page(group, limit, self.cover(), false)
    }

    async fn get(&self, request: IdRequest) -> Result<Track, Failure> {
        let id = validated_id(&request.id)?;
        if media::is_library_id(id) {
            let payload = self.client.get_library_song(media::resource_id(id)).await?;
            let mut track = parse::song_from_payload(&payload, self.cover())?;
            // `get` must echo the exact input ID, not the bare upstream ID.
            track.id = id.to_string();
            Ok(track)
        } else {
            let payload = self.client.get_song(id).await?;
            parse::song_from_payload(&payload, self.cover())
        }
    }

    async fn get_many(&self, request: BatchRequest) -> Result<TrackBatch, Failure> {
        let mut items = Vec::with_capacity(request.ids.len());
        for id in request.ids {
            let outcome = match self.get(IdRequest { id: id.clone() }).await {
                Ok(track) => TrackOutcome {
                    id: id.clone(),
                    track: Some(track),
                    failure: None,
                },
                Err(failure) => TrackOutcome {
                    id: id.clone(),
                    track: None,
                    failure: Some(failure),
                },
            };
            items.push(outcome);
        }
        Ok(TrackBatch { items })
    }

    async fn resolve(
        &self,
        request: ResolveRequest,
    ) -> Result<crate::models::MediaResource, Failure> {
        let id = validated_id(&request.id)?;
        media::resolve(&self.client, id, &request.quality, &self.settings).await
    }

    async fn resolve_drm(&self, request: DrmResolveRequest) -> Result<ResolvedMedia, Failure> {
        drm::resolve(&self.client, &request, &self.settings).await
    }

    async fn exchange_license(&self, request: LicenseRequest) -> Result<LicenseResponse, Failure> {
        drm::exchange_license(&self.client, request).await
    }

    async fn lyrics(&self, request: IdRequest) -> Result<Lyrics, Failure> {
        let id = validated_id(&request.id)?;
        // The lyrics endpoint is catalog-only; a library track must resolve to
        // its catalog counterpart instead of producing an invalid path.
        let catalog_id = if media::is_library_id(id) {
            let payload = self.client.get_library_song(media::resource_id(id)).await?;
            parse::catalog_id_from_library(&payload)
                .ok_or_else(|| not_found("Library song has no catalog counterpart"))?
        } else {
            id.to_string()
        };
        let payload = self.client.get_song_lyrics(&catalog_id).await?;
        parse::lyrics(&payload, id)
    }

    async fn search_playlists(&self, request: SearchRequest) -> Result<PlaylistPage, Failure> {
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .search(
                request.keyword.trim(),
                "playlists",
                limit,
                request.offset.max(0),
            )
            .await?;
        let group = parse::search_group(&payload, "playlists")?;
        parse::playlist_page(group, limit, self.cover())
    }

    async fn playlist(&self, request: IdRequest) -> Result<Playlist, Failure> {
        let id = validated_id(&request.id)?;
        let library = media::is_library_id(id);
        let payload = if library {
            self.client
                .get_library_playlist(media::resource_id(id))
                .await?
        } else {
            self.client.get_playlist(id).await?
        };
        let mut playlist = parse::playlist_from_payload(&payload, self.cover())?;
        if library {
            // `getPlaylistTracks` must receive the same namespaced identity.
            playlist.id = id.to_string();
        }
        Ok(playlist)
    }

    async fn playlist_tracks(&self, request: PagedIdRequest) -> Result<TrackPage, Failure> {
        let id = validated_id(&request.id)?;
        let limit = clamp_limit(request.limit);
        let offset = request.offset.max(0);
        let payload = if media::is_library_id(id) {
            self.client
                .get_library_playlist_tracks(media::resource_id(id), limit, offset)
                .await?
        } else {
            self.client.get_playlist_tracks(id, limit, offset).await?
        };
        parse::track_page(&payload, limit, self.cover(), media::is_library_id(id))
    }

    async fn user_playlists(&self, request: PagedIdRequest) -> Result<PlaylistPage, Failure> {
        // `request.id` may carry the host account key; library playlists are
        // always scoped to the current credential.
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .get_user_playlists(limit, request.offset.max(0))
            .await?;
        parse::playlist_page(&payload, limit, self.cover())
    }

    async fn search_albums(&self, request: SearchRequest) -> Result<AlbumPage, Failure> {
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .search(
                request.keyword.trim(),
                "albums",
                limit,
                request.offset.max(0),
            )
            .await?;
        let group = parse::search_group(&payload, "albums")?;
        parse::album_page(group, limit, self.cover())
    }

    async fn album(&self, request: IdRequest) -> Result<AlbumDetails, Failure> {
        let id = validated_id(&request.id)?;
        if media::is_library_id(id) {
            return Err(unsupported(
                "Library album detail is not exposed by this provider",
            ));
        }
        let payload = self.client.get_album(id).await?;
        let mut details = parse::album_details(&payload, self.cover(), false)?;
        if details.tracks.is_empty() {
            // `include=tracks` is not requested by the album endpoint; page them
            // separately rather than returning a metadata-only album.
            let tracks = self.client.get_album_tracks(id, 500, 0).await?;
            details.tracks = parse::track_page(&tracks, 500, self.cover(), false)?.items;
        }
        Ok(details)
    }

    async fn search_artists(
        &self,
        request: SearchRequest,
    ) -> Result<crate::models::ArtistPage, Failure> {
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .search(
                request.keyword.trim(),
                "artists",
                limit,
                request.offset.max(0),
            )
            .await?;
        let group = parse::search_group(&payload, "artists")?;
        parse::artist_page(group, limit, self.cover())
    }

    async fn artist(&self, request: IdRequest) -> Result<crate::models::CatalogArtist, Failure> {
        let id = validated_id(&request.id)?;
        let payload = self.client.get_artist(id).await?;
        parse::artist_from_payload(&payload, self.cover())
    }

    async fn artist_tracks(&self, request: PagedIdRequest) -> Result<TrackPage, Failure> {
        let id = validated_id(&request.id)?;
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .get_artist_tracks(id, limit, request.offset.max(0))
            .await?;
        parse::track_page(&payload, limit, self.cover(), false)
    }

    async fn artist_albums(&self, request: PagedIdRequest) -> Result<AlbumPage, Failure> {
        let id = validated_id(&request.id)?;
        let limit = clamp_limit(request.limit);
        let payload = self
            .client
            .get_artist_albums(id, limit, request.offset.max(0))
            .await?;
        parse::album_page(&payload, limit, self.cover())
    }

    async fn account(&self) -> Result<AccountSession, Failure> {
        if !self.client.is_logged_in() {
            return Ok(AccountSession {
                loggedIn: false,
                userId: None,
                nickname: None,
                avatarUrl: None,
            });
        }
        let payload = self.client.get_account().await?;
        let data = payload
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        Ok(AccountSession {
            loggedIn: true,
            userId: data
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            nickname: data
                .and_then(|item| item.get("attributes"))
                .and_then(|attributes| attributes.get("displayName"))
                .and_then(Value::as_str)
                .map(str::to_string),
            avatarUrl: None,
        })
    }
}

fn clamp_limit(limit: i32) -> i32 {
    limit.clamp(1, 500)
}

fn validated_id(id: &str) -> Result<&str, Failure> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 1024 {
        return Err(not_found("Invalid Apple Music resource ID"));
    }
    Ok(trimmed)
}

fn skip_no_argument(reader: &mut WireReader) -> Result<(), Failure> {
    BytesProbe::read(reader)?;
    reader.finish()?;
    Ok(())
}

/// `Bytes` is a length-prefixed blob; no-argument operations send an empty one.
struct BytesProbe;

impl BytesProbe {
    fn read(reader: &mut WireReader) -> Result<(), crate::wire::WireError> {
        let _ = reader.bytes()?;
        Ok(())
    }
}

fn encode<F>(write: F) -> Result<Vec<u8>, Failure>
where
    F: FnOnce(&mut WireWriter),
{
    let mut writer = WireWriter::new();
    write(&mut writer);
    writer
        .finish()
        .map_err(|_| invalid_response("Provider response exceeded the wire limit"))
}

// Capability constants are referenced so typos surface at compile time.
const _: () = {
    let _ = capabilities::SEARCH;
};
