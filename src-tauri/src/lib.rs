use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use keyring::Entry;
use librespot::{
    connect::{ConnectConfig, LoadRequest, LoadRequestOptions, Spirc},
    core::{
        authentication::Credentials,
        config::{DeviceType, SessionConfig},
        session::Session,
    },
    playback::{
        audio_backend,
        config::{AudioFormat, PlayerConfig},
        mixer::{self, MixerConfig},
        player::{Player, PlayerEvent},
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::Mutex,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State};

mod audio_analysis;
mod catalog;

const MAGNET_SPOTIFY_CLIENT_ID: &str = "49a6085899814912912b8174495e7702";
const SPOTIFY_REDIRECT_URI: &str = "http://127.0.0.1:5002/auth/spotify/callback";
const SPOTIFY_SCOPES: &str = "user-read-private user-read-email user-library-read user-library-modify playlist-read-private playlist-read-collaborative playlist-modify-public playlist-modify-private streaming";
const SPOTIFY_KEYRING_SERVICE: &str = "magnet";
const LEGACY_SPOTIFY_KEYRING_SERVICE: &str = "magnet-player";
const SPOTIFY_KEYRING_ACCOUNT: &str = "spotify-refresh-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Track {
    id: String,
    title: String,
    artists: Vec<String>,
    album: String,
    duration_ms: u32,
    saved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Playlist {
    id: String,
    name: String,
    owner: String,
    track_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueEntry {
    queue_id: String,
    track: Track,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackState {
    track: Option<Track>,
    position_ms: u32,
    playing: bool,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RepeatMode {
    Off,
    Context,
    Track,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ViewId {
    Library,
    Search,
    Queue,
    Browse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSnapshot {
    revision: u64,
    view: ViewId,
    library: Vec<Track>,
    playlists: Vec<Playlist>,
    queue: Vec<QueueEntry>,
    playback: PlaybackState,
    playlist_context_id: Option<String>,
    authenticated: bool,
    catalog_loading: bool,
    spotify_configured: bool,
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Preferences {
    foreground_hidden: bool,
}

struct PendingLogin {
    state: String,
    verifier: String,
}

#[allow(dead_code)]
struct SpotifySession {
    access_token: String,
    refresh_token: Option<String>,
    expires_at_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyAuthResult {
    authenticated: bool,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    request_id: String,
    items: serde_json::Value,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct SpotifyProfile {
    display_name: Option<String>,
    id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
// The renderer is TypeScript, so action payload fields arrive in camelCase
// (`trackId`, `positionMs`). Keep Rust enum variants in snake_case while
// accepting the renderer's native field names.
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum PlayerAction {
    PlayTrack {
        track_id: String,
        #[serde(default)]
        track: Option<Track>,
    },
    PlayPlaylist {
        playlist_id: String,
    },
    SetSaved {
        track_id: String,
        #[serde(default)]
        track: Option<Track>,
        saved: bool,
    },
    AddToPlaylist {
        track_id: String,
        playlist_id: String,
    },
    TogglePlayback,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    SetVolume {
        volume: f32,
    },
    ToggleShuffle,
    CycleRepeat,
    Enqueue {
        track_id: String,
        #[serde(default)]
        track: Option<Track>,
    },
    MoveQueueItem {
        queue_id: String,
        to_index: usize,
    },
    RemoveQueueItem {
        queue_id: String,
    },
    ClearQueue,
    SetView {
        view: ViewId,
    },
}

struct AppState {
    snapshot: Mutex<AppSnapshot>,
    preferences: Mutex<Preferences>,
    pending_login: Mutex<Option<PendingLogin>>,
    oauth_callback_ready: Mutex<bool>,
    spotify_session: Mutex<Option<SpotifySession>>,
    native_player: Mutex<Option<Spirc>>,
    audio_analyzer: Mutex<Option<audio_analysis::AudioAnalyzerHandle>>,
    queue_sequence: Mutex<u64>,
}

impl AppState {
    fn new() -> Self {
        let library = demo_library().into_iter().take(3).collect::<Vec<_>>();
        let now_playing = library.first().cloned();
        Self {
            snapshot: Mutex::new(AppSnapshot {
                revision: 0,
                view: ViewId::Library,
                queue: library
                    .iter()
                    .take(3)
                    .enumerate()
                    .map(|(index, track)| QueueEntry {
                        queue_id: format!("demo-{}", index + 1),
                        track: track.clone(),
                    })
                    .collect(),
                library,
                playlists: Vec::new(),
                playback: PlaybackState {
                    track: now_playing,
                    position_ms: 194_000,
                    playing: true,
                    volume: 0.65,
                    shuffle: false,
                    repeat: RepeatMode::Off,
                },
                playlist_context_id: None,
                authenticated: false,
                catalog_loading: false,
                spotify_configured: true,
                message: Some(
                    "Desktop shell preview — connect Spotify to link your library.".into(),
                ),
            }),
            preferences: Mutex::new(Preferences {
                foreground_hidden: false,
            }),
            pending_login: Mutex::new(None),
            oauth_callback_ready: Mutex::new(false),
            spotify_session: Mutex::new(None),
            native_player: Mutex::new(None),
            audio_analyzer: Mutex::new(None),
            queue_sequence: Mutex::new(0),
        }
    }
}

#[tauri::command]
fn snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state
        .snapshot
        .lock()
        .map(|snapshot| snapshot.clone())
        .map_err(|_| "Player state is unavailable.".into())
}

#[tauri::command]
fn preferences(state: State<'_, AppState>) -> Result<Preferences, String> {
    state
        .preferences
        .lock()
        .map(|preferences| preferences.clone())
        .map_err(|_| "Preferences are unavailable.".into())
}

// Spotify access tokens are short-lived. Catalog requests run independently of
// the native player, so refresh them at this boundary before issuing a search
// or opening a playlist. Keeping the session lock during refresh deliberately
// serializes concurrent catalog calls and avoids a token-refresh stampede.
fn catalog_access_token(state: &AppState) -> Result<String, String> {
    const REFRESH_SKEW_MS: u128 = 60_000;

    let mut session = state
        .spotify_session
        .lock()
        .map_err(|_| "Spotify session is unavailable.".to_string())?;
    let current = session
        .as_mut()
        .ok_or_else(|| "Connect Spotify before using its catalog.".to_string())?;

    if current.expires_at_ms.saturating_sub(now_ms()) > REFRESH_SKEW_MS {
        return Ok(current.access_token.clone());
    }

    let refresh_token = current
        .refresh_token
        .clone()
        .ok_or_else(|| "Spotify needs you to reconnect before using its catalog.".to_string())?;
    let token = refresh_access_token(&spotify_client_id(), &refresh_token)?;
    let reusable_refresh_token = token
        .refresh_token
        .as_deref()
        .unwrap_or(&refresh_token)
        .to_owned();
    if token.refresh_token.is_some() {
        persist_refresh_token(&reusable_refresh_token)?;
    }

    current.access_token = token.access_token;
    current.refresh_token = Some(reusable_refresh_token);
    current.expires_at_ms = now_ms().saturating_add(u128::from(token.expires_in) * 1_000);
    Ok(current.access_token.clone())
}

#[tauri::command]
fn set_preferences(state: State<'_, AppState>, preferences: Preferences) -> Result<(), String> {
    let mut current = state
        .preferences
        .lock()
        .map_err(|_| "Preferences are unavailable.".to_string())?;
    *current = preferences;
    Ok(())
}

#[tauri::command]
fn dispatch(
    app: AppHandle,
    state: State<'_, AppState>,
    action: PlayerAction,
) -> Result<AppSnapshot, String> {
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    let mut next = snapshot.clone();
    apply_player_action(&state, &mut next, &action)?;

    // Keep input ordering deterministic: the visible state is committed only
    // after librespot has accepted the matching command. The command path is
    // local and non-blocking, while the renderer is free to draw its immediate
    // preview without waiting for this response.
    if next.authenticated && action_needs_native_player(&action) {
        dispatch_to_native_player(
            &state,
            &action,
            &next.playback,
            next.playlist_context_id.is_some(),
        )
        .map_err(|error| format!("Local Spotify playback: {error}"))?;
        if let Some(message) = player_feedback(&action, &next.playback) {
            next.message = Some(message);
        }
    }

    *snapshot = next;
    drop(snapshot);
    publish_player_state(&app);
    state
        .snapshot
        .lock()
        .map(|snapshot| snapshot.clone())
        .map_err(|_| "Player state is unavailable.".to_string())
}

#[tauri::command]
fn search_spotify(
    state: State<'_, AppState>,
    query: String,
    kind: catalog::SearchKind,
    cursor: Option<String>,
) -> Result<SearchResponse, String> {
    let access_token = catalog_access_token(state.inner())?;
    let page = catalog::search_page(
        &reqwest::blocking::Client::new(),
        &access_token,
        &query,
        kind,
        cursor.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let (items, next_cursor) = match page {
        catalog::SearchPage::Tracks { items, next_cursor } => (
            serde_json::to_value(items).map_err(|error| error.to_string())?,
            next_cursor,
        ),
        catalog::SearchPage::Playlists { items, next_cursor } => (
            serde_json::to_value(items).map_err(|error| error.to_string())?,
            next_cursor,
        ),
    };
    Ok(SearchResponse {
        request_id: format!("search-{}", now_ms()),
        items,
        next_cursor,
    })
}

#[tauri::command]
fn playlist_tracks(state: State<'_, AppState>, playlist_id: String) -> Result<Vec<Track>, String> {
    let access_token = catalog_access_token(state.inner())?;
    match catalog::playlist_tracks(
        &reqwest::blocking::Client::new(),
        &access_token,
        &playlist_id,
    ) {
        Err(catalog::CatalogError::Spotify { status, .. })
            if status == reqwest::StatusCode::FORBIDDEN =>
        {
            Err("Spotify does not expose this playlist's track list to desktop clients. Double-click to play it.".into())
        }
        result => result.map_err(|error| error.to_string()),
    }
}

fn publish_player_state(app: &AppHandle) {
    let next = {
        let state = app.state::<AppState>();
        let Ok(mut snapshot) = state.snapshot.lock() else {
            return;
        };
        snapshot.revision = snapshot.revision.wrapping_add(1);
        snapshot.clone()
    };
    let _ = app.emit("player-state", next);
}

fn track_id_from_uri(uri: &str) -> &str {
    uri.rsplit(':').next().unwrap_or(uri)
}

fn sync_native_track(snapshot: &mut AppSnapshot, track_uri: &str) {
    let track_id = track_id_from_uri(track_uri);
    if let Some(track) = snapshot
        .library
        .iter()
        .find(|track| track.id == track_id)
        .cloned()
    {
        snapshot.playback.track = Some(track);
    }
}

fn advance_queue_after_track(app: &AppHandle) {
    let state = app.state::<AppState>();
    let track = {
        let Ok(mut snapshot) = state.snapshot.lock() else {
            return;
        };
        let Some(entry) = snapshot.queue.first().cloned() else {
            return;
        };
        snapshot.queue.remove(0);
        snapshot.playback.track = Some(entry.track.clone());
        snapshot.playback.position_ms = 0;
        snapshot.playback.playing = true;
        snapshot.message = Some(format!("Playing {}.", entry.track.title));
        entry.track
    };

    if let Err(error) = dispatch_to_native_player(
        &state,
        &PlayerAction::PlayTrack {
            track_id: track.id.clone(),
            track: Some(track.clone()),
        },
        &PlaybackState {
            track: Some(track),
            position_ms: 0,
            playing: true,
            volume: 0.0,
            shuffle: false,
            repeat: RepeatMode::Off,
        },
        false,
    ) {
        if let Ok(mut snapshot) = state.snapshot.lock() {
            snapshot.playback.playing = false;
            snapshot.message = Some(format!("Could not advance the queue: {error}"));
        }
    }
    publish_player_state(app);
}

fn handle_player_event(app: &AppHandle, event: PlayerEvent) {
    if matches!(event, PlayerEvent::EndOfTrack { .. }) {
        advance_queue_after_track(app);
        return;
    }
    let state = app.state::<AppState>();
    let Ok(mut snapshot) = state.snapshot.lock() else {
        return;
    };
    match event {
        PlayerEvent::Loading {
            track_id,
            position_ms,
            ..
        } => {
            sync_native_track(&mut snapshot, &track_id.to_string());
            snapshot.playback.position_ms = position_ms;
            snapshot.playback.playing = false;
            snapshot.message = Some("Loading track…".into());
        }
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => {
            sync_native_track(&mut snapshot, &track_id.to_string());
            snapshot.playback.position_ms = position_ms;
            snapshot.playback.playing = true;
            snapshot.message = snapshot
                .playback
                .track
                .as_ref()
                .map(|track| format!("Playing {}.", track.title));
        }
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => {
            sync_native_track(&mut snapshot, &track_id.to_string());
            snapshot.playback.position_ms = position_ms;
            snapshot.playback.playing = false;
            snapshot.message = Some("Paused.".into());
        }
        PlayerEvent::PositionChanged {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        } => {
            sync_native_track(&mut snapshot, &track_id.to_string());
            snapshot.playback.position_ms = position_ms;
        }
        PlayerEvent::VolumeChanged { volume } => {
            snapshot.playback.volume = f32::from(volume) / f32::from(u16::MAX);
        }
        PlayerEvent::ShuffleChanged { shuffle } => snapshot.playback.shuffle = shuffle,
        PlayerEvent::RepeatChanged { context, track } => {
            snapshot.playback.repeat = if track {
                RepeatMode::Track
            } else if context {
                RepeatMode::Context
            } else {
                RepeatMode::Off
            };
        }
        PlayerEvent::Unavailable { track_id, .. } => {
            sync_native_track(&mut snapshot, &track_id.to_string());
            snapshot.playback.playing = false;
            snapshot.message = Some("This track is unavailable for playback.".into());
        }
        PlayerEvent::Stopped { .. } => {
            snapshot.playback.playing = false;
        }
        _ => {}
    }
    drop(snapshot);
    publish_player_state(app);
}

fn next_queue_id(state: &AppState) -> Result<String, String> {
    let mut sequence = state
        .queue_sequence
        .lock()
        .map_err(|_| "Queue state is unavailable.".to_string())?;
    *sequence = sequence.wrapping_add(1);
    Ok(format!("q-{sequence}"))
}

fn apply_player_action(
    state: &AppState,
    snapshot: &mut AppSnapshot,
    action: &PlayerAction,
) -> Result<(), String> {
    match action {
        PlayerAction::SetView { view } => snapshot.view = *view,
        PlayerAction::PlayTrack { track_id, track } => {
            if let Some(track) = track
                .as_ref()
                .filter(|track| track.id == *track_id)
                .cloned()
                .or_else(|| {
                    snapshot
                        .library
                        .iter()
                        .find(|known| known.id == *track_id)
                        .cloned()
                })
            {
                snapshot.playback.track = Some(track);
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
                snapshot.playlist_context_id = None;
            }
        }
        PlayerAction::PlayPlaylist { playlist_id } => {
            snapshot.playlist_context_id = Some(playlist_id.clone());
            snapshot.playback.position_ms = 0;
            snapshot.playback.playing = true;
        }
        PlayerAction::SetSaved {
            track_id,
            track,
            saved,
        } => {
            if snapshot.authenticated {
                let access_token = catalog_access_token(state)?;
                catalog::set_track_saved(
                    &reqwest::blocking::Client::new(),
                    &access_token,
                    track_id,
                    *saved,
                )
                .map_err(|error| error.to_string())?;
            }
            if *saved {
                if let Some(existing) = snapshot.library.iter_mut().find(|item| item.id == *track_id) {
                    existing.saved = Some(true);
                } else if let Some(mut track) = track.clone() {
                    track.saved = Some(true);
                    snapshot.library.insert(0, track);
                }
                snapshot.message = Some("Saved to your library.".into());
            } else {
                snapshot.library.retain(|item| item.id != *track_id);
                snapshot.message = Some("Removed from your library.".into());
            }
        }
        PlayerAction::AddToPlaylist {
            track_id,
            playlist_id,
        } => {
            if !snapshot.authenticated {
                return Err("Connect Spotify to edit playlists.".into());
            }
            let access_token = catalog_access_token(state)?;
            catalog::add_track_to_playlist(
                &reqwest::blocking::Client::new(),
                &access_token,
                playlist_id,
                track_id,
            )
            .map_err(|error| error.to_string())?;
            let name = snapshot
                .playlists
                .iter()
                .find(|playlist| playlist.id == *playlist_id)
                .map(|playlist| playlist.name.as_str())
                .unwrap_or("playlist");
            snapshot.message = Some(format!("Added to {name}."));
        }
        PlayerAction::TogglePlayback => snapshot.playback.playing = !snapshot.playback.playing,
        PlayerAction::Seek { position_ms } => snapshot.playback.position_ms = *position_ms,
        PlayerAction::SetVolume { volume } => snapshot.playback.volume = volume.clamp(0.0, 1.0),
        PlayerAction::ToggleShuffle => snapshot.playback.shuffle = !snapshot.playback.shuffle,
        PlayerAction::CycleRepeat => {
            snapshot.playback.repeat = match snapshot.playback.repeat {
                RepeatMode::Off => RepeatMode::Context,
                RepeatMode::Context => RepeatMode::Track,
                RepeatMode::Track => RepeatMode::Off,
            }
        }
        PlayerAction::Enqueue { track_id, track } => {
            if let Some(track) = track
                .as_ref()
                .filter(|track| track.id == *track_id)
                .cloned()
                .or_else(|| {
                    snapshot
                        .library
                        .iter()
                        .find(|known| known.id == *track_id)
                        .cloned()
                })
            {
                snapshot.queue.push(QueueEntry {
                    queue_id: next_queue_id(state)?,
                    track,
                });
            }
        }
        PlayerAction::MoveQueueItem { queue_id, to_index } => {
            if let Some(index) = snapshot
                .queue
                .iter()
                .position(|entry| entry.queue_id == *queue_id)
            {
                let entry = snapshot.queue.remove(index);
                let destination = (*to_index).min(snapshot.queue.len());
                snapshot.queue.insert(destination, entry);
            }
        }
        PlayerAction::RemoveQueueItem { queue_id } => {
            snapshot.queue.retain(|entry| entry.queue_id != *queue_id);
        }
        PlayerAction::ClearQueue => snapshot.queue.clear(),
        PlayerAction::Next => {
            if let Some(entry) = snapshot.queue.first().cloned() {
                snapshot.queue.remove(0);
                snapshot.playback.track = Some(entry.track);
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
                snapshot.playlist_context_id = None;
                return Ok(());
            }
            if snapshot.playlist_context_id.is_some() {
                return Ok(());
            }
            let current_index = snapshot
                .playback
                .track
                .as_ref()
                .and_then(|current| {
                    snapshot
                        .library
                        .iter()
                        .position(|track| track.id == current.id)
                })
                .unwrap_or(0);
            let count = snapshot.library.len();
            if count > 0 {
                let next_index = (current_index + 1) % count;
                snapshot.playback.track = Some(snapshot.library[next_index].clone());
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
            }
        }
        PlayerAction::Previous => {
            if snapshot.playlist_context_id.is_some() {
                return Ok(());
            }
            let current_index = snapshot
                .playback
                .track
                .as_ref()
                .and_then(|current| {
                    snapshot
                        .library
                        .iter()
                        .position(|track| track.id == current.id)
                })
                .unwrap_or(0);
            let count = snapshot.library.len();
            if count > 0 {
                let previous_index = (current_index + count - 1) % count;
                snapshot.playback.track = Some(snapshot.library[previous_index].clone());
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
            }
        }
    }
    Ok(())
}

fn action_needs_native_player(action: &PlayerAction) -> bool {
    !matches!(
        action,
        PlayerAction::SetView { .. }
            | PlayerAction::Enqueue { .. }
            | PlayerAction::SetSaved { .. }
            | PlayerAction::AddToPlaylist { .. }
            | PlayerAction::MoveQueueItem { .. }
            | PlayerAction::RemoveQueueItem { .. }
            | PlayerAction::ClearQueue
    )
}

fn player_feedback(action: &PlayerAction, playback: &PlaybackState) -> Option<String> {
    match action {
        PlayerAction::PlayTrack { .. }
        | PlayerAction::PlayPlaylist { .. }
        | PlayerAction::Next
        | PlayerAction::Previous => playback
            .track
            .as_ref()
            .map(|track| format!("Playing {}.", track.title)),
        PlayerAction::TogglePlayback => Some(if playback.playing {
            "Playing.".into()
        } else {
            "Paused.".into()
        }),
        _ => None,
    }
}

fn dispatch_to_native_player(
    state: &AppState,
    action: &PlayerAction,
    playback: &PlaybackState,
    playlist_context_active: bool,
) -> Result<(), String> {
    let native = state
        .native_player
        .lock()
        .map_err(|_| "audio backend is unavailable".to_string())?;
    let player = native
        .as_ref()
        .ok_or_else(|| "is starting — wait a moment, then try again".to_string())?;
    let command =
        |result: Result<(), librespot::core::Error>| result.map_err(|error| error.to_string());

    match action {
        PlayerAction::PlayTrack { track_id, .. } => {
            command(player.activate())?;
            command(player.load(LoadRequest::from_tracks(
                vec![format!("spotify:track:{track_id}")],
                LoadRequestOptions {
                    start_playing: true,
                    ..Default::default()
                },
            )))
        }
        PlayerAction::PlayPlaylist { playlist_id } => {
            command(player.activate())?;
            command(player.load(LoadRequest::from_context_uri(
                format!("spotify:playlist:{playlist_id}"),
                LoadRequestOptions {
                    start_playing: true,
                    ..Default::default()
                },
            )))
        }
        PlayerAction::TogglePlayback => {
            if playback.playing {
                command(player.play())
            } else {
                command(player.pause())
            }
        }
        PlayerAction::Next if playlist_context_active => command(player.next()),
        PlayerAction::Previous if playlist_context_active => command(player.prev()),
        // Tracks selected from the library are loaded as single-item contexts.
        // Calling Connect's next/previous on that context leaves audio where it
        // is while the UI advances, which is the most noticeable source of
        // "janky" transport. Load the exact track the reducer selected instead.
        PlayerAction::Next | PlayerAction::Previous => {
            let track = playback
                .track
                .as_ref()
                .ok_or_else(|| "there is no track selected".to_string())?;
            command(player.activate())?;
            command(player.load(LoadRequest::from_tracks(
                vec![format!("spotify:track:{}", track.id)],
                LoadRequestOptions {
                    start_playing: true,
                    ..Default::default()
                },
            )))
        }
        PlayerAction::Seek { position_ms } => command(player.set_position_ms(*position_ms)),
        PlayerAction::SetVolume { volume } => command(
            player.set_volume((volume.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16),
        ),
        PlayerAction::ToggleShuffle => command(player.shuffle(playback.shuffle)),
        PlayerAction::CycleRepeat => {
            command(player.repeat(playback.repeat == RepeatMode::Context))?;
            command(player.repeat_track(playback.repeat == RepeatMode::Track))
        }
        PlayerAction::Enqueue { .. }
        | PlayerAction::SetSaved { .. }
        | PlayerAction::AddToPlaylist { .. }
        | PlayerAction::MoveQueueItem { .. }
        | PlayerAction::RemoveQueueItem { .. }
        | PlayerAction::ClearQueue
        | PlayerAction::SetView { .. } => Ok(()),
    }
}

#[tauri::command]
fn begin_login(_app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let client_id = spotify_client_id();
    let verifier = random_urlsafe(64)?;
    let csrf_state = random_urlsafe(24)?;
    if !*state
        .oauth_callback_ready
        .lock()
        .map_err(|_| "Spotify callback status is unavailable.".to_string())?
    {
        return Err("Magnet's Spotify callback server is unavailable on 127.0.0.1:5002. Restart Magnet and make sure no other app is using that port.".into());
    }

    {
        let mut pending = state
            .pending_login
            .lock()
            .map_err(|_| "Spotify authorization is unavailable.".to_string())?;
        *pending = Some(PendingLogin {
            state: csrf_state.clone(),
            verifier: verifier.clone(),
        });
    }

    let authorization_url = spotify_authorization_url(&client_id, &csrf_state, &verifier);
    tauri_plugin_opener::open_url(&authorization_url, None::<String>)
        .map_err(|error| format!("Could not open Spotify sign-in: {error}"))?;

    Ok(())
}

#[tauri::command]
fn export_diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let path = root.join("magnet-diagnostics.json");
    let snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?
        .clone();
    let report = serde_json::json!({
        "generated_at_unix_ms": now_ms(),
        "app": "magnet",
        "version": env!("CARGO_PKG_VERSION"),
        "spotify_configured": snapshot.spotify_configured,
        "authenticated": snapshot.authenticated,
        "queue_length": snapshot.queue.len(),
        "note": "No credentials, access tokens, playback URLs, or raw audio are included."
    });
    fs::write(
        &path,
        serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn spotify_client_id() -> String {
    option_env!("MAGNET_SPOTIFY_CLIENT_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| MAGNET_SPOTIFY_CLIENT_ID.to_owned())
}

fn spotify_authorization_url(client_id: &str, csrf_state: &str, verifier: &str) -> String {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", SPOTIFY_REDIRECT_URI)
        .append_pair("scope", SPOTIFY_SCOPES)
        .append_pair("state", csrf_state)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .finish();
    format!("https://accounts.spotify.com/authorize?{query}")
}

fn random_urlsafe(byte_length: usize) -> Result<String, String> {
    let mut bytes = vec![0_u8; byte_length];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("Could not start Spotify authorization securely: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn serve_oauth_callbacks(app: AppHandle, listener: TcpListener, client_id: String) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let result = complete_oauth_callback(&app, stream, &client_id);
                let (authenticated, message) = match result {
                    Ok(message) => (true, message),
                    Err(error) => (false, error),
                };
                update_oauth_status(&app, authenticated, message);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(80));
            }
            Err(error) => {
                update_oauth_status(&app, false, format!("Spotify callback failed: {error}"));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn complete_oauth_callback(
    app: &AppHandle,
    mut stream: TcpStream,
    client_id: &str,
) -> Result<String, String> {
    let mut request_line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut request_line)
        .map_err(|error| format!("Could not read Spotify callback: {error}"))?;
    let request_target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "Spotify callback was malformed.".to_string())?;
    let query = request_target
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let pending = app
        .state::<AppState>()
        .pending_login
        .lock()
        .map_err(|_| "Spotify authorization is unavailable.".to_string())?
        .take()
        .ok_or_else(|| "No Spotify sign-in was waiting for this callback.".to_string())?;

    if params.get("state") != Some(&pending.state) {
        write_callback_page(
            &mut stream,
            false,
            "Sign-in rejected: the Spotify callback did not match this request.",
        );
        return Err(
            "Spotify sign-in was rejected because its callback state did not match.".into(),
        );
    }
    if let Some(error) = params.get("error") {
        write_callback_page(
            &mut stream,
            false,
            "Spotify sign-in was not completed. You can close this tab and return to Magnet.",
        );
        return Err(format!("Spotify sign-in was not completed: {error}."));
    }
    let code = params
        .get("code")
        .ok_or_else(|| "Spotify did not return an authorization code.".to_string())?;
    // Acknowledge the browser before making any network calls. Token/profile
    // requests can take seconds; leaving the redirect socket open until then
    // makes browsers report an empty or failed localhost response.
    write_callback_page(
        &mut stream,
        true,
        "Sign-in received. Magnet is finishing the connection — you can close this tab.",
    );
    let token = exchange_code_for_token(client_id, code, &pending.verifier)?;
    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        "Spotify did not return a reusable refresh token. Try connecting again.".to_string()
    })?;
    persist_refresh_token(&refresh_token)?;
    let native_access_token = token.access_token.clone();
    let expires_at_ms = now_ms().saturating_add(u128::from(token.expires_in) * 1_000);
    *app.state::<AppState>()
        .spotify_session
        .lock()
        .map_err(|_| "Spotify session storage is unavailable.".to_string())? =
        Some(SpotifySession {
            access_token: token.access_token.clone(),
            refresh_token: Some(refresh_token),
            expires_at_ms,
        });
    // From this point forward the account is linked. Never leave the sample
    // catalog visible merely because Spotify throttles an optional profile or
    // library request immediately after authorization.
    begin_catalog_loading(app)?;

    // Token exchange is the only work the callback needs to wait for. Profile
    // and library endpoints can be slow or rate-limited; doing them here left
    // the desktop UI looking permanently disconnected after a valid redirect.
    let catalog_app = app.clone();
    thread::spawn(move || {
        let library_result = spotify_saved_tracks(&token.access_token);
        let playlists_result = spotify_playlists(&token.access_token);
        let profile_result = spotify_profile(&token.access_token);
        let account_name = profile_result
            .as_ref()
            .ok()
            .and_then(|profile| profile.display_name.clone().or_else(|| Some(profile.id.clone())))
            .unwrap_or_else(|| "your Spotify account".into());
        let mut loaded = Vec::new();
        let mut issues = Vec::new();
        if let Err(error) = &profile_result {
            issues.push(format!("profile: {error}"));
        }
        if let (Ok(profile), Ok(library), Ok(playlists)) = (&profile_result, &library_result, &playlists_result) {
            if let Err(error) = store_collection_cache(&catalog_app, &profile.id, library, playlists) {
                issues.push(format!("offline cache: {error}"));
            }
        }
        match library_result {
            Ok(library) => {
                loaded.push(format!("{} saved tracks", library.len()));
                if let Err(error) = sync_library_snapshot(&catalog_app, library) {
                    issues.push(format!("saved tracks: {error}"));
                }
            }
            Err(error) => issues.push(format!("saved tracks: {error}")),
        }
        match playlists_result {
            Ok(playlists) => {
                loaded.push(format!("{} playlists", playlists.len()));
                if let Err(error) = sync_playlists_snapshot(&catalog_app, playlists) {
                    issues.push(format!("playlists: {error}"));
                }
            }
            Err(error) => issues.push(format!("playlists: {error}")),
        }
        let mut message = format!("Spotify connected as {account_name}");
        if !loaded.is_empty() {
            message.push_str(&format!(" · {} loaded", loaded.join(", ")));
        }
        if !issues.is_empty() {
            message.push_str(&format!(" · could not load {}", issues.join("; ")));
        }
        message.push('.');
        finish_catalog_loading(&catalog_app);
        update_oauth_status(&catalog_app, true, message);
    });

    let native_app = app.clone();
    tauri::async_runtime::spawn(async move {
        match start_native_player(native_app.clone(), native_access_token).await {
            Ok(()) => update_native_player_status(&native_app, "Native Spotify audio is ready on this device.".into()),
            Err(error) => update_native_player_status(&native_app, format!("Native Spotify audio could not start: {error}")),
        }
    });

    Ok("Spotify connected. Loading your library…".into())
}

fn exchange_code_for_token(
    client_id: &str,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, String> {
    let response = reqwest::blocking::Client::new()
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", SPOTIFY_REDIRECT_URI),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|error| format!("Spotify token exchange failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Spotify token exchange was refused ({}).",
            response.status()
        ));
    }
    response
        .json::<TokenResponse>()
        .map_err(|error| format!("Spotify returned an invalid token response: {error}"))
}

fn refresh_access_token(client_id: &str, refresh_token: &str) -> Result<TokenResponse, String> {
    let response = reqwest::blocking::Client::new()
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .map_err(|error| format!("Spotify session refresh failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Spotify session refresh was refused ({}).",
            response.status()
        ));
    }
    response
        .json::<TokenResponse>()
        .map_err(|error| format!("Spotify returned an invalid refresh response: {error}"))
}

fn spotify_refresh_token_entry() -> Result<Entry, String> {
    Entry::new(SPOTIFY_KEYRING_SERVICE, SPOTIFY_KEYRING_ACCOUNT)
        .map_err(|error| format!("Windows Credential Manager is unavailable: {error}"))
}

fn persist_refresh_token(refresh_token: &str) -> Result<(), String> {
    spotify_refresh_token_entry()?
        .set_password(refresh_token)
        .map_err(|error| format!("Could not save the Spotify session securely: {error}"))
}

fn stored_refresh_token() -> Result<Option<String>, String> {
    match spotify_refresh_token_entry()?.get_password() {
        Ok(token) if !token.trim().is_empty() => return Ok(Some(token)),
        Ok(_) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(format!("Could not read the saved Spotify session: {error}")),
    }

    // Preserve a pre-rename session. The next refresh writes it to Magnet's
    // credential entry, so users do not have to complete OAuth again merely
    // because the desktop app was renamed.
    let legacy = Entry::new(LEGACY_SPOTIFY_KEYRING_SERVICE, SPOTIFY_KEYRING_ACCOUNT)
        .map_err(|error| format!("Windows Credential Manager is unavailable: {error}"))?;
    match legacy.get_password() {
        Ok(token) if !token.trim().is_empty() => {
            persist_refresh_token(&token)?;
            Ok(Some(token))
        }
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Could not read the saved Spotify session: {error}")),
    }
}

fn restore_spotify_session(app: AppHandle) {
    thread::spawn(move || {
        let refresh_token = match stored_refresh_token() {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(error) => {
                if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
                    snapshot.message = Some(error);
                }
                return;
            }
        };

        if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
            snapshot.message = Some("Restoring Spotify session…".into());
        }
        let _ = begin_catalog_loading(&app);

        let result = refresh_access_token(&spotify_client_id(), &refresh_token).and_then(|token| {
            let rotated_refresh_token = token.refresh_token.as_deref().unwrap_or(&refresh_token);
            if token.refresh_token.is_some() {
                persist_refresh_token(rotated_refresh_token)?;
            }
            let native_access_token = token.access_token.clone();
            let expires_at_ms = now_ms().saturating_add(u128::from(token.expires_in) * 1_000);

            *app.state::<AppState>()
                .spotify_session
                .lock()
                .map_err(|_| "Spotify session storage is unavailable.".to_string())? =
                Some(SpotifySession {
                    access_token: token.access_token.clone(),
                refresh_token: Some(rotated_refresh_token.to_owned()),
                expires_at_ms,
            });

            // A refresh token is sufficient proof of a linked account. Do not
            // demote it back to the connect screen if optional profile or
            // catalog calls are throttled immediately afterwards.
            update_oauth_status(&app, true, "Spotify session restored. Loading your library…".into());

            let library_result = spotify_saved_tracks(&token.access_token);
            let playlists_result = spotify_playlists(&token.access_token);
            let profile_result = spotify_profile(&token.access_token);
            let account_name = profile_result
                .as_ref()
                .ok()
                .and_then(|profile| profile.display_name.clone().or_else(|| Some(profile.id.clone())))
                .unwrap_or_else(|| "your Spotify account".into());
            if let Ok(profile) = &profile_result {
                if let Ok(cache) = load_collection_cache(&app, &profile.id) {
                    sync_library_snapshot(&app, cache.tracks)?;
                    sync_playlists_snapshot(&app, cache.playlists)?;
                }
            }

            let mut loaded = Vec::new();
            let mut issues = Vec::new();
            if let Err(error) = &profile_result {
                issues.push(format!("profile: {error}"));
            }
            if let (Ok(profile), Ok(library), Ok(playlists)) = (&profile_result, &library_result, &playlists_result) {
                if let Err(error) = store_collection_cache(&app, &profile.id, library, playlists) {
                    issues.push(format!("offline cache: {error}"));
                }
            }
            match library_result {
                Ok(library) => {
                    loaded.push(format!("{} saved tracks", library.len()));
                    sync_library_snapshot(&app, library)?;
                }
                Err(error) => issues.push(format!("saved tracks: {error}")),
            }
            match playlists_result {
                Ok(playlists) => {
                    loaded.push(format!("{} playlists", playlists.len()));
                    sync_playlists_snapshot(&app, playlists)?;
                }
                Err(error) => issues.push(format!("playlists: {error}")),
            }

            let mut message = format!("Spotify session restored as {account_name}");
            if !loaded.is_empty() {
                message.push_str(&format!(" · {} loaded", loaded.join(", ")));
            }
            if !issues.is_empty() {
                message.push_str(&format!(" · could not load {}", issues.join("; ")));
            }
            message.push('.');
            finish_catalog_loading(&app);
            update_oauth_status(&app, true, message.clone());

            let native_app = app.clone();
            tauri::async_runtime::spawn(async move {
                match start_native_player(native_app.clone(), native_access_token).await {
                    Ok(()) => update_native_player_status(
                        &native_app,
                        "Native Spotify audio is ready on this device.".into(),
                    ),
                    Err(error) => update_native_player_status(
                        &native_app,
                        format!("Native Spotify audio could not start: {error}"),
                    ),
                }
            });
            Ok(())
        });

        if let Err(error) = result {
            if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
                snapshot.authenticated = false;
                snapshot.catalog_loading = false;
                snapshot.message = Some(format!(
                    "Saved Spotify session could not be restored: {error}"
                ));
            }
        }
    });
}

fn spotify_profile(access_token: &str) -> Result<SpotifyProfile, String> {
    let response = spotify_http_client()?
        .get("https://api.spotify.com/v1/me")
        .bearer_auth(access_token)
        .send()
        .map_err(|error| format!("Could not load your Spotify profile: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Spotify profile request was refused ({}).",
            response.status()
        ));
    }
    response
        .json::<SpotifyProfile>()
        .map_err(|error| format!("Spotify returned an invalid profile response: {error}"))
}

fn collection_cache_path(app: &AppHandle, spotify_user_id: &str) -> Result<PathBuf, String> {
    let digest = Sha256::digest(spotify_user_id.as_bytes());
    let suffix = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    app.path()
        .app_data_dir()
        .map(|root| root.join("catalog").join(format!("{suffix}.json")))
        .map_err(|error| format!("Could not locate the Spotify collection cache: {error}"))
}

fn load_collection_cache(
    app: &AppHandle,
    spotify_user_id: &str,
) -> Result<catalog::CollectionCache, String> {
    let path = collection_cache_path(app, spotify_user_id)?;
    catalog::load_cache(&path, Some(spotify_user_id)).map_err(|error| error.to_string())
}

fn store_collection_cache(
    app: &AppHandle,
    spotify_user_id: &str,
    tracks: &[Track],
    playlists: &[Playlist],
) -> Result<(), String> {
    let path = collection_cache_path(app, spotify_user_id)?;
    let cache = catalog::CollectionCache::new(spotify_user_id, tracks.to_vec(), playlists.to_vec())
        .map_err(|error| error.to_string())?;
    catalog::store_cache(&path, &cache).map_err(|error| error.to_string())
}

fn spotify_saved_tracks(access_token: &str) -> Result<Vec<Track>, String> {
    const BACKOFF_SECONDS: [u64; 4] = [5, 15, 30, 60];
    let mut last_error = None;
    for delay in BACKOFF_SECONDS.into_iter().chain(std::iter::once(0)) {
        match catalog::saved_tracks(&spotify_http_client()?, access_token) {
            Ok(tracks) => return Ok(tracks),
            Err(error) => {
                let message = error.to_string();
                if !message.contains("429") || delay == 0 {
                    return Err(message);
                }
                last_error = Some(message);
                thread::sleep(Duration::from_secs(delay));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "Spotify saved tracks could not be loaded.".into()))
}

fn spotify_playlists(access_token: &str) -> Result<Vec<Playlist>, String> {
    catalog::playlists(&spotify_http_client()?, access_token)
        .map_err(|error| error.to_string())
}

fn spotify_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|error| format!("Could not initialize Spotify networking: {error}"))
}

fn sync_library_snapshot(app: &AppHandle, library: Vec<Track>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    // The demo shell starts with a visible queue; a real Spotify session starts
    // with an intentionally empty Magnet-owned session queue.
    snapshot.queue.clear();
    snapshot.playback.track = library.first().cloned();
    snapshot.playback.position_ms = 0;
    snapshot.playback.playing = false;
    snapshot.library = library;
    drop(snapshot);
    publish_player_state(app);
    Ok(())
}

fn begin_catalog_loading(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    snapshot.catalog_loading = true;
    snapshot.queue.clear();
    snapshot.library.clear();
    snapshot.playlists.clear();
    snapshot.playback.track = None;
    snapshot.playback.position_ms = 0;
    snapshot.playback.playing = false;
    drop(snapshot);
    publish_player_state(app);
    Ok(())
}

fn finish_catalog_loading(app: &AppHandle) {
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        snapshot.catalog_loading = false;
    }
    publish_player_state(app);
}

fn sync_playlists_snapshot(app: &AppHandle, playlists: Vec<Playlist>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    snapshot.playlists = playlists;
    drop(snapshot);
    publish_player_state(app);
    Ok(())
}

async fn start_native_player(app: AppHandle, access_token: String) -> Result<(), String> {
    let session = Session::new(SessionConfig::default(), None);
    let sink_builder = audio_backend::find(None)
        .ok_or_else(|| "no Windows audio sink is available".to_string())?;
    let mixer_builder =
        mixer::find(None).ok_or_else(|| "no software volume mixer is available".to_string())?;
    let mixer = mixer_builder(MixerConfig::default())
        .map_err(|error| format!("could not initialize the audio mixer: {error}"))?;
    let visual_app = app.clone();
    let (analysis_input, analyzer) = audio_analysis::spawn_audio_analyzer(
        audio_analysis::AnalysisConfig::default(),
        move |frame| {
            // The WebGL nebula may be disabled while the lightweight FFT
            // histogram remains visible over the wallpaper. PCM analysis is
            // therefore independent from the renderer preference.
            let _ = visual_app.emit("visual-frame", frame);
        },
    )?;
    let mut player_config = PlayerConfig::default();
    player_config.position_update_interval = Some(Duration::from_millis(250));
    let player = Player::new(
        player_config,
        session.clone(),
        mixer.get_soft_volume(),
        move || {
            Box::new(audio_analysis::AnalysisSink::new(
                sink_builder(None, AudioFormat::default()),
                analysis_input,
            )) as _
        },
    );
    let mut player_events = player.get_player_event_channel();
    let event_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = player_events.recv().await {
            handle_player_event(&event_app, event);
        }
    });
    let connect_config = ConnectConfig {
        name: "Magnet".into(),
        device_type: DeviceType::Computer,
        ..Default::default()
    };
    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session,
        Credentials::with_access_token(access_token),
        player,
        mixer,
    )
    .await
    .map_err(|error| format!("Spotify Connect rejected the session: {error}"))?;

    *app.state::<AppState>()
        .native_player
        .lock()
        .map_err(|_| "could not store the native audio session".to_string())? = Some(spirc);
    *app.state::<AppState>()
        .audio_analyzer
        .lock()
        .map_err(|_| "could not retain the audio analyzer".to_string())? = Some(analyzer);

    tauri::async_runtime::spawn(async move { spirc_task.await });
    Ok(())
}

fn update_native_player_status(app: &AppHandle, message: String) {
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        // Catalog failures are actionable (especially missing playlist scopes),
        // while the native-ready notification is informational. Do not hide the
        // former when the audio session comes online a moment later.
        let has_catalog_warning = snapshot
            .message
            .as_deref()
            .is_some_and(|current| current.contains("could not load"));
        if !has_catalog_warning {
            snapshot.message = Some(message.clone());
        }
    }
    publish_player_state(app);
    let _ = app.emit(
        "spotify-auth-complete",
        SpotifyAuthResult {
            authenticated: true,
            message,
        },
    );
}

fn write_callback_page(stream: &mut TcpStream, success: bool, message: &str) {
    let tone = if success { "#5ce8ad" } else { "#f4b46f" };
    let body = format!("<!doctype html><title>Magnet</title><body style=\"margin:0;display:grid;min-height:100vh;place-items:center;background:#070b11;color:#ecf5f0;font:16px ui-monospace,Consolas,monospace\"><main style=\"max-width:36rem;padding:2rem\"><p style=\"color:{tone}\">magnet</p><h1>{message}</h1></main></body>");
    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    let _ = stream.write_all(response.as_bytes());
}

fn update_oauth_status(app: &AppHandle, authenticated: bool, message: String) {
    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
        snapshot.authenticated = authenticated;
        snapshot.spotify_configured = true;
        snapshot.message = Some(message.clone());
    }
    publish_player_state(app);
    let _ = app.emit(
        "spotify-auth-complete",
        SpotifyAuthResult {
            authenticated,
            message,
        },
    );
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            // Keep the callback server alive for the whole app lifetime. OAuth
            // pages routinely sit open for more than a few minutes, so a
            // one-shot listener can disappear before Spotify redirects back.
            match TcpListener::bind("127.0.0.1:5002") {
                Ok(listener) => match listener.set_nonblocking(true) {
                    Ok(()) => {
                        if let Ok(mut ready) = app.state::<AppState>().oauth_callback_ready.lock() {
                            *ready = true;
                        }
                        let callback_app = app.handle().clone();
                        let client_id = spotify_client_id();
                        thread::spawn(move || serve_oauth_callbacks(callback_app, listener, client_id));
                    }
                    Err(error) => {
                        if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
                            snapshot.message = Some(format!("Spotify callback setup failed: {error}"));
                        }
                    }
                },
                Err(error) => {
                    if let Ok(mut snapshot) = app.state::<AppState>().snapshot.lock() {
                        snapshot.message = Some(format!("Spotify callback server unavailable on 127.0.0.1:5002: {error}"));
                    }
                }
            }
            // The refresh token lives in Windows Credential Manager, never in
            // the app data folder. Restore it off the UI thread so launch stays
            // immediate even when Spotify is slow to answer.
            restore_spotify_session(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            preferences,
            set_preferences,
            dispatch,
            search_spotify,
            playlist_tracks,
            begin_login,
            export_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("error while running Magnet");
}

fn demo_library() -> Vec<Track> {
    [
        ("Nabokov", "Fontaines D.C.", "5:21"),
        (
            "You Don't Need Anyone",
            "oskar med k, kris., mondaé",
            "2:38",
        ),
        ("This Modern Love", "Bloc Party", "4:26"),
        ("Cross The Street", "Junior Varsity", "2:47"),
        ("Truth", "Flycatcher", "3:12"),
        ("East Village", "Spacey Jane", "3:31"),
        ("Sunscreen", "Ax and the Hatchetmen", "2:54"),
        ("Great Pretender", "Dominic Fike", "2:51"),
        ("What Do We Ever Really Know?", "Balu Brigada", "3:53"),
        ("Warm Nights", "Royel Otis", "3:31"),
        ("SPEND THE WEEK", "Laszewo", "2:45"),
        ("Hotel Room", "Ax and the Hatchetmen", "2:28"),
        ("Golden Gate Girl", "Balu Brigada", "3:17"),
        ("Mother", "Royel Otis", "3:13"),
        ("New York", "Junior Varsity", "2:43"),
        ("Right As Rain", "MisterWives", "4:17"),
    ]
    .iter()
    .enumerate()
    .map(|(index, (title, artist, duration))| Track {
        id: (index + 1).to_string(),
        title: (*title).into(),
        artists: artist.split(", ").map(str::to_owned).collect(),
        album: "August 2026".into(),
        duration_ms: parse_duration(duration),
        saved: Some(index == 0),
    })
    .collect()
}

fn parse_duration(value: &str) -> u32 {
    let parts: Vec<u32> = value
        .split(':')
        .filter_map(|part| part.parse().ok())
        .collect();
    if parts.len() != 2 {
        return 0;
    }
    (parts[0] * 60 + parts[1]) * 1000
}

#[cfg(test)]
mod tests {
    use super::PlayerAction;

    #[test]
    fn player_actions_accept_renderer_camel_case_fields() {
        let play = serde_json::from_str::<PlayerAction>(r#"{"type":"play_track","trackId":"abc"}"#)
            .expect("play_track action should deserialize");
        assert!(matches!(play, PlayerAction::PlayTrack { track_id, .. } if track_id == "abc"));

        let seek = serde_json::from_str::<PlayerAction>(r#"{"type":"seek","positionMs":1200}"#)
            .expect("seek action should deserialize");
        assert!(matches!(seek, PlayerAction::Seek { position_ms } if position_ms == 1200));
    }
}
