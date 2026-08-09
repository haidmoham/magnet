//! Spotify catalog loading, search, and durable collection caching.
//!
//! Integration contract for `lib.rs`:
//!
//! ```ignore
//! mod catalog;
//!
//! let client = reqwest::blocking::Client::new();
//! let tracks = catalog::saved_tracks(&client, access_token)?;
//! let playlists = catalog::playlists(&client, access_token)?;
//!
//! let page = catalog::search_page(
//!     &client,
//!     access_token,
//!     query,
//!     catalog::SearchKind::Tracks,
//!     None,
//! )?;
//! // Pass `page.next_cursor()` back as `cursor` to load the next page.
//!
//! let cache = catalog::CollectionCache::new(user_id, tracks, playlists);
//! catalog::store_cache(&cache_path, &cache)?;
//! let restored = catalog::load_cache(&cache_path, Some(user_id))?;
//! ```
//!
//! The parent module's existing `Track` and `Playlist` types are deliberately
//! reused so integration does not introduce a second application model.

use super::{Playlist, Track};
use reqwest::{blocking::Client, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashSet,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const COLLECTION_CACHE_VERSION: u32 = 1;
const SAVED_TRACKS_URL: &str = "https://api.spotify.com/v1/me/tracks?limit=50";
const PLAYLISTS_URL: &str = "https://api.spotify.com/v1/me/playlists?limit=50";
const SEARCH_URL: &str = "https://api.spotify.com/v1/search";
// Spotify's live Search endpoint currently rejects the catalogue pagination
// limit for application clients above ten. Keep the page small and use its
// validated `next` cursor for explicit pagination in the client.
const SEARCH_PAGE_SIZE: &str = "10";
const MAX_PAGES: usize = 10_000;
const MAX_ERROR_BODY_CHARS: usize = 512;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) enum CatalogError {
    Http(reqwest::Error),
    Spotify { status: StatusCode, message: String },
    InvalidContinuationUrl(String),
    PaginationLoop(String),
    TooManyPages,
    EmptySearch,
    Io(io::Error),
    Json(serde_json::Error),
    UnsupportedCacheVersion(u32),
    CacheUserMismatch,
    Clock,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "Spotify request failed: {error}"),
            Self::Spotify { status, message } if message.is_empty() => {
                write!(formatter, "Spotify refused the request ({status})")
            }
            Self::Spotify { status, message } => {
                write!(
                    formatter,
                    "Spotify refused the request ({status}): {message}"
                )
            }
            Self::InvalidContinuationUrl(url) => {
                write!(
                    formatter,
                    "Spotify returned an unsafe continuation URL: {url}"
                )
            }
            Self::PaginationLoop(url) => {
                write!(formatter, "Spotify repeated a continuation URL: {url}")
            }
            Self::TooManyPages => write!(formatter, "Spotify pagination exceeded its safety limit"),
            Self::EmptySearch => write!(formatter, "Search text cannot be empty"),
            Self::Io(error) => write!(formatter, "Catalog cache I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "Catalog JSON was invalid: {error}"),
            Self::UnsupportedCacheVersion(version) => {
                write!(formatter, "Catalog cache version {version} is unsupported")
            }
            Self::CacheUserMismatch => {
                write!(
                    formatter,
                    "Catalog cache belongs to a different Spotify account"
                )
            }
            Self::Clock => write!(formatter, "The system clock is earlier than the Unix epoch"),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for CatalogError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

impl From<io::Error> for CatalogError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CatalogError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CollectionCache {
    pub schema_version: u32,
    pub spotify_user_id: String,
    pub fetched_at_ms: u128,
    pub tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
}

impl CollectionCache {
    pub(super) fn new(
        spotify_user_id: impl Into<String>,
        tracks: Vec<Track>,
        playlists: Vec<Playlist>,
    ) -> Result<Self, CatalogError> {
        let fetched_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CatalogError::Clock)?
            .as_millis();
        Ok(Self {
            schema_version: COLLECTION_CACHE_VERSION,
            spotify_user_id: spotify_user_id.into(),
            fetched_at_ms,
            tracks,
            playlists,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SearchKind {
    Tracks,
    Playlists,
}

impl SearchKind {
    fn spotify_value(self) -> &'static str {
        match self {
            Self::Tracks => "track",
            Self::Playlists => "playlist",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum SearchPage {
    Tracks {
        items: Vec<Track>,
        #[serde(rename = "nextCursor")]
        next_cursor: Option<String>,
    },
    Playlists {
        items: Vec<Playlist>,
        #[serde(rename = "nextCursor")]
        next_cursor: Option<String>,
    },
}

impl SearchPage {
    pub(super) fn next_cursor(&self) -> Option<&str> {
        match self {
            Self::Tracks { next_cursor, .. } | Self::Playlists { next_cursor, .. } => {
                next_cursor.as_deref()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Page<T> {
    #[serde(default)]
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SavedTrackItem {
    track: Option<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct PlaylistTrackItem {
    item: Option<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct SpotifyTrack {
    id: Option<String>,
    name: String,
    #[serde(default)]
    artists: Vec<SpotifyArtist>,
    album: SpotifyAlbum,
    duration_ms: u32,
}

#[derive(Debug, Deserialize)]
struct SpotifyArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyAlbum {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyPlaylist {
    id: Option<String>,
    name: String,
    owner: SpotifyPlaylistOwner,
    // Spotify's current simplified-playlist response calls this `items`,
    // while older responses used `tracks`. Accept both so a schema rollout
    // cannot silently erase the entire browse library.
    #[serde(alias = "items")]
    tracks: SpotifyPlaylistTracks,
}

#[derive(Debug, Deserialize)]
struct SpotifyPlaylistOwner {
    display_name: Option<String>,
    id: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyPlaylistTracks {
    total: u32,
}

#[derive(Debug, Deserialize)]
struct TrackSearchResponse {
    tracks: Page<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct PlaylistSearchResponse {
    playlists: Page<Option<SpotifyPlaylist>>,
}

pub(super) fn saved_tracks(
    client: &Client,
    access_token: &str,
) -> Result<Vec<Track>, CatalogError> {
    paginate(
        SAVED_TRACKS_URL,
        |url| get_json::<Page<SavedTrackItem>>(client, access_token, url),
        |item| item.track.and_then(|track| into_track(track, Some(true))),
    )
}

pub(super) fn playlists(
    client: &Client,
    access_token: &str,
) -> Result<Vec<Playlist>, CatalogError> {
    paginate(
        PLAYLISTS_URL,
        |url| get_json::<Page<Option<SpotifyPlaylist>>>(client, access_token, url),
        |playlist| playlist.and_then(into_playlist),
    )
}

pub(super) fn playlist_tracks(
    client: &Client,
    access_token: &str,
    playlist_id: &str,
) -> Result<Vec<Track>, CatalogError> {
    if playlist_id.is_empty()
        || playlist_id.len() > 128
        || !playlist_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(CatalogError::InvalidContinuationUrl(
            "invalid Spotify playlist identifier".into(),
        ));
    }
    let url = format!("https://api.spotify.com/v1/playlists/{playlist_id}/items?limit=50");
    paginate(
        &url,
        |url| get_json::<Page<PlaylistTrackItem>>(client, access_token, url),
        |item| item.item.and_then(|track| into_track(track, None)),
    )
}

pub(super) fn search_page(
    client: &Client,
    access_token: &str,
    query: &str,
    kind: SearchKind,
    cursor: Option<&str>,
) -> Result<SearchPage, CatalogError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(CatalogError::EmptySearch);
    }

    let url = match cursor {
        Some(cursor) => validate_search_url(cursor, kind)?.to_string(),
        None => Url::parse_with_params(
            SEARCH_URL,
            [
                ("q", query),
                ("type", kind.spotify_value()),
                ("limit", SEARCH_PAGE_SIZE),
                ("market", "from_token"),
                ("include_external", "audio"),
            ],
        )
        .expect("the static Spotify search URL is valid")
        .to_string(),
    };

    match kind {
        SearchKind::Tracks => {
            let response: TrackSearchResponse = get_json(client, access_token, &url)?;
            Ok(SearchPage::Tracks {
                items: response
                    .tracks
                    .items
                    .into_iter()
                    .filter_map(|track| into_track(track, None))
                    .collect(),
                next_cursor: validate_optional_search_continuation(response.tracks.next, kind)?,
            })
        }
        SearchKind::Playlists => {
            let response: PlaylistSearchResponse = get_json(client, access_token, &url)?;
            Ok(SearchPage::Playlists {
                items: response
                    .playlists
                    .items
                    .into_iter()
                    .flatten()
                    .filter_map(into_playlist)
                    .collect(),
                next_cursor: validate_optional_search_continuation(response.playlists.next, kind)?,
            })
        }
    }
}

pub(super) fn load_cache(
    path: &Path,
    expected_spotify_user_id: Option<&str>,
) -> Result<CollectionCache, CatalogError> {
    let reader = BufReader::new(File::open(path)?);
    let cache: CollectionCache = serde_json::from_reader(reader)?;
    if cache.schema_version != COLLECTION_CACHE_VERSION {
        return Err(CatalogError::UnsupportedCacheVersion(cache.schema_version));
    }
    if expected_spotify_user_id.is_some_and(|expected| cache.spotify_user_id != expected) {
        return Err(CatalogError::CacheUserMismatch);
    }
    Ok(cache)
}

pub(super) fn store_cache(path: &Path, cache: &CollectionCache) -> Result<(), CatalogError> {
    if cache.schema_version != COLLECTION_CACHE_VERSION {
        return Err(CatalogError::UnsupportedCacheVersion(cache.schema_version));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    let temporary = temporary_path(path);
    let write_result = (|| -> Result<(), CatalogError> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, cache)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        atomic_replace(&temporary, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn into_track(track: SpotifyTrack, saved: Option<bool>) -> Option<Track> {
    Some(Track {
        id: track.id?,
        title: track.name,
        artists: track
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect(),
        album: track.album.name,
        duration_ms: track.duration_ms,
        saved,
    })
}

fn into_playlist(playlist: SpotifyPlaylist) -> Option<Playlist> {
    Some(Playlist {
        id: playlist.id?,
        name: playlist.name,
        owner: playlist.owner.display_name.unwrap_or(playlist.owner.id),
        track_count: playlist.tracks.total,
    })
}

fn paginate<Item, Output, Fetch, Convert>(
    first_url: &str,
    mut fetch: Fetch,
    mut convert: Convert,
) -> Result<Vec<Output>, CatalogError>
where
    Fetch: FnMut(&str) -> Result<Page<Item>, CatalogError>,
    Convert: FnMut(Item) -> Option<Output>,
{
    let mut next = Some(validate_spotify_url(first_url)?.to_string());
    let mut visited = HashSet::new();
    let mut output = Vec::new();

    for _ in 0..MAX_PAGES {
        let Some(url) = next.take() else {
            return Ok(output);
        };
        note_page(&url, &mut visited)?;
        let page = fetch(&url)?;
        output.extend(page.items.into_iter().filter_map(&mut convert));
        next = validate_optional_continuation(page.next)?;
    }

    Err(CatalogError::TooManyPages)
}

fn get_json<T: DeserializeOwned>(
    client: &Client,
    access_token: &str,
    url: &str,
) -> Result<T, CatalogError> {
    let url = validate_spotify_url(url)?;
    let response = client.get(url).bearer_auth(access_token).send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        let message = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
        return Err(CatalogError::Spotify { status, message });
    }
    response.json().map_err(CatalogError::Http)
}

fn validate_optional_continuation(
    continuation: Option<String>,
) -> Result<Option<String>, CatalogError> {
    continuation
        .map(|url| validate_spotify_url(&url).map(|validated| validated.to_string()))
        .transpose()
}

fn validate_optional_search_continuation(
    continuation: Option<String>,
    kind: SearchKind,
) -> Result<Option<String>, CatalogError> {
    continuation
        .map(|url| validate_search_url(&url, kind).map(|validated| validated.to_string()))
        .transpose()
}

fn validate_search_url(value: &str, kind: SearchKind) -> Result<Url, CatalogError> {
    let parsed = validate_spotify_url(value)?;
    let is_expected_search = parsed.path() == "/v1/search"
        && parsed
            .query_pairs()
            .any(|(key, value)| key == "type" && value == kind.spotify_value());
    if is_expected_search {
        Ok(parsed)
    } else {
        Err(CatalogError::InvalidContinuationUrl(value.to_string()))
    }
}

fn validate_spotify_url(value: &str) -> Result<Url, CatalogError> {
    let parsed =
        Url::parse(value).map_err(|_| CatalogError::InvalidContinuationUrl(value.to_string()))?;
    let safe = parsed.scheme() == "https"
        && parsed.host_str() == Some("api.spotify.com")
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if safe {
        Ok(parsed)
    } else {
        Err(CatalogError::InvalidContinuationUrl(value.to_string()))
    }
}

fn note_page(url: &str, visited: &mut HashSet<String>) -> Result<(), CatalogError> {
    let normalized = validate_spotify_url(url)?.to_string();
    if visited.insert(normalized.clone()) {
        Ok(())
    } else {
        Err(CatalogError::PaginationLoop(normalized))
    }
}

fn temporary_path(destination: &Path) -> PathBuf {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("spotify-collection-v1.json");
    destination.with_file_name(format!(
        ".{name}.{}.{}.{}.tmp",
        std::process::id(),
        nanos,
        sequence
    ))
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both paths are stable, NUL-terminated UTF-16 buffers for the
    // duration of the call. The API does not retain either pointer.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("Track {id}"),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            duration_ms: 123_000,
            saved: Some(true),
        }
    }

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: format!("Playlist {id}"),
            owner: "Owner".into(),
            track_count: 7,
        }
    }

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "magnet-catalog-{name}-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn nullable_and_unavailable_playlists_are_skipped_individually() {
        let json = r#"{
          "items": [
            null,
            {"id":"good","name":"Good","owner":{"display_name":null,"id":"owner"},"tracks":{"total":3}},
            {"id":null,"name":"Unavailable","owner":{"display_name":"Owner","id":"owner"},"tracks":{"total":0}}
          ],
          "next": null
        }"#;
        let page: Page<Option<SpotifyPlaylist>> = serde_json::from_str(json).unwrap();
        let decoded: Vec<_> = page
            .items
            .into_iter()
            .flatten()
            .filter_map(into_playlist)
            .collect();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id, "good");
        assert_eq!(decoded[0].owner, "owner");
    }

    #[test]
    fn current_spotify_playlist_items_shape_is_accepted() {
        let json = r#"{
          "items": [
            {"id":"good","name":"Good","owner":{"display_name":"Owner","id":"owner"},"items":{"href":"https://api.spotify.com/v1/playlists/good/items","total":12}}
          ],
          "next": null
        }"#;
        let page: Page<Option<SpotifyPlaylist>> = serde_json::from_str(json).unwrap();
        let playlist = page
            .items
            .into_iter()
            .flatten()
            .next()
            .and_then(into_playlist)
            .unwrap();
        assert_eq!(playlist.track_count, 12);
    }

    #[test]
    fn continuation_rejects_non_spotify_hosts_before_bearer_use() {
        assert!(matches!(
            validate_spotify_url("https://evil.example/v1/me/tracks"),
            Err(CatalogError::InvalidContinuationUrl(_))
        ));
        assert!(matches!(
            validate_spotify_url("http://api.spotify.com/v1/me/tracks"),
            Err(CatalogError::InvalidContinuationUrl(_))
        ));
        assert!(validate_spotify_url("https://api.spotify.com/v1/me/tracks?offset=50").is_ok());
    }

    #[test]
    fn search_continuations_cannot_switch_catalog_endpoints_or_kinds() {
        assert!(validate_search_url(
            "https://api.spotify.com/v1/search?q=radiohead&type=track&limit=50&offset=50",
            SearchKind::Tracks,
        )
        .is_ok());
        assert!(matches!(
            validate_search_url(
                "https://api.spotify.com/v1/search?q=radiohead&type=playlist&limit=50&offset=50",
                SearchKind::Tracks,
            ),
            Err(CatalogError::InvalidContinuationUrl(_))
        ));
        assert!(matches!(
            validate_search_url(
                "https://api.spotify.com/v1/me/tracks?limit=50&offset=50",
                SearchKind::Tracks,
            ),
            Err(CatalogError::InvalidContinuationUrl(_))
        ));
    }

    #[test]
    fn pagination_loop_is_detected() {
        let url = "https://api.spotify.com/v1/me/tracks?limit=50";
        let mut visited = HashSet::new();
        note_page(url, &mut visited).unwrap();
        assert!(matches!(
            note_page(url, &mut visited),
            Err(CatalogError::PaginationLoop(_))
        ));
    }

    #[test]
    fn pagination_collects_every_page_and_filters_items_individually() {
        let second = "https://api.spotify.com/v1/me/playlists?limit=50&offset=50";
        let third = "https://api.spotify.com/v1/me/playlists?limit=50&offset=100";
        let mut pages = VecDeque::from([
            Page {
                items: vec![Some(playlist_payload("one")), None],
                next: Some(second.into()),
            },
            Page {
                items: vec![Some(playlist_payload("two"))],
                next: Some(third.into()),
            },
            Page {
                items: vec![Some(playlist_payload("three"))],
                next: None,
            },
        ]);
        let mut requested = Vec::new();
        let decoded = paginate(
            PLAYLISTS_URL,
            |url| {
                requested.push(url.to_string());
                Ok(pages.pop_front().unwrap())
            },
            |playlist| playlist.and_then(into_playlist),
        )
        .unwrap();

        assert_eq!(requested, [PLAYLISTS_URL, second, third]);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[2].id, "three");
    }

    #[test]
    fn cache_round_trip_and_replacement_are_lossless() {
        let directory = test_directory("round-trip");
        let path = directory.join("spotify-collection-v1.json");
        let first = CollectionCache {
            schema_version: COLLECTION_CACHE_VERSION,
            spotify_user_id: "user".into(),
            fetched_at_ms: 1,
            tracks: vec![track("one")],
            playlists: vec![playlist("one")],
        };
        store_cache(&path, &first).unwrap();
        let restored = load_cache(&path, Some("user")).unwrap();
        assert_eq!(restored.tracks[0].id, "one");

        let second = CollectionCache {
            schema_version: COLLECTION_CACHE_VERSION,
            spotify_user_id: "user".into(),
            fetched_at_ms: 2,
            tracks: vec![track("two")],
            playlists: vec![],
        };
        store_cache(&path, &second).unwrap();
        let restored = load_cache(&path, Some("user")).unwrap();
        assert_eq!(restored.fetched_at_ms, 2);
        assert_eq!(restored.tracks[0].id, "two");
        assert!(restored.playlists.is_empty());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cache_rejects_wrong_version_and_account() {
        let directory = test_directory("validation");
        let path = directory.join("spotify-collection-v1.json");
        let cache = CollectionCache {
            schema_version: COLLECTION_CACHE_VERSION,
            spotify_user_id: "first".into(),
            fetched_at_ms: 1,
            tracks: vec![],
            playlists: vec![],
        };
        store_cache(&path, &cache).unwrap();
        assert!(matches!(
            load_cache(&path, Some("second")),
            Err(CatalogError::CacheUserMismatch)
        ));

        let unsupported = CollectionCache {
            schema_version: COLLECTION_CACHE_VERSION + 1,
            ..cache
        };
        assert!(matches!(
            store_cache(&path, &unsupported),
            Err(CatalogError::UnsupportedCacheVersion(_))
        ));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn search_page_serializes_to_frontend_friendly_shape() {
        let page = SearchPage::Tracks {
            items: vec![track("one")],
            next_cursor: Some("https://api.spotify.com/v1/search?offset=25".into()),
        };
        let json = serde_json::to_value(page).unwrap();
        assert_eq!(json["kind"], "tracks");
        assert_eq!(json["items"][0]["durationMs"], 123_000);
        assert!(json["nextCursor"].is_string());
    }

    fn playlist_payload(id: &str) -> SpotifyPlaylist {
        SpotifyPlaylist {
            id: Some(id.into()),
            name: format!("Playlist {id}"),
            owner: SpotifyPlaylistOwner {
                display_name: None,
                id: "owner".into(),
            },
            tracks: SpotifyPlaylistTracks { total: 1 },
        }
    }
}
