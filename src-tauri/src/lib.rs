use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::Mutex,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State};

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
struct PlaybackState {
    track: Option<Track>,
    position_ms: u32,
    playing: bool,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
    view: ViewId,
    library: Vec<Track>,
    queue: Vec<Track>,
    playback: PlaybackState,
    authenticated: bool,
    spotify_configured: bool,
    message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VisualIntensity {
    Calm,
    Standard,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VisualQuality {
    Auto,
    Eco,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Preferences {
    visuals_enabled: bool,
    intensity: VisualIntensity,
    quality: VisualQuality,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisualFrame {
    timestamp_ms: u128,
    bass: f32,
    mid: f32,
    treble: f32,
    energy: f32,
    onset: f32,
    stereo: f32,
    silence: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PlayerAction {
    PlayTrack { track_id: String },
    TogglePlayback,
    Next,
    Previous,
    Seek { position_ms: u32 },
    SetVolume { volume: f32 },
    ToggleShuffle,
    CycleRepeat,
    Enqueue { track_id: String },
    SetView { view: ViewId },
}

struct AppState {
    snapshot: Mutex<AppSnapshot>,
    preferences: Mutex<Preferences>,
}

impl AppState {
    fn new() -> Self {
        let library = demo_library();
        let now_playing = library.first().cloned();
        Self {
            snapshot: Mutex::new(AppSnapshot {
                view: ViewId::Library,
                queue: library[..6].to_vec(),
                library,
                playback: PlaybackState {
                    track: now_playing,
                    position_ms: 194_000,
                    playing: true,
                    volume: 0.65,
                    shuffle: false,
                    repeat: RepeatMode::Off,
                },
                authenticated: false,
                spotify_configured: spotify_client_id().is_some(),
                message: Some(
                    "Desktop shell preview — configure a Spotify app ID to connect playback."
                        .into(),
                ),
            }),
            preferences: Mutex::new(Preferences {
                visuals_enabled: true,
                intensity: VisualIntensity::Standard,
                quality: VisualQuality::Auto,
            }),
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
fn dispatch(state: State<'_, AppState>, action: PlayerAction) -> Result<AppSnapshot, String> {
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    match action {
        PlayerAction::SetView { view } => snapshot.view = view,
        PlayerAction::PlayTrack { track_id } => {
            if let Some(track) = snapshot
                .library
                .iter()
                .find(|track| track.id == track_id)
                .cloned()
            {
                snapshot.playback.track = Some(track);
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
            }
        }
        PlayerAction::TogglePlayback => snapshot.playback.playing = !snapshot.playback.playing,
        PlayerAction::Seek { position_ms } => snapshot.playback.position_ms = position_ms,
        PlayerAction::SetVolume { volume } => snapshot.playback.volume = volume.clamp(0.0, 1.0),
        PlayerAction::ToggleShuffle => snapshot.playback.shuffle = !snapshot.playback.shuffle,
        PlayerAction::CycleRepeat => {
            snapshot.playback.repeat = match snapshot.playback.repeat {
                RepeatMode::Off => RepeatMode::Context,
                RepeatMode::Context => RepeatMode::Track,
                RepeatMode::Track => RepeatMode::Off,
            }
        }
        PlayerAction::Enqueue { track_id } => {
            if let Some(track) = snapshot
                .library
                .iter()
                .find(|track| track.id == track_id)
                .cloned()
            {
                snapshot.queue.push(track);
            }
        }
        PlayerAction::Next | PlayerAction::Previous => {
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
                let next_index = match action {
                    PlayerAction::Next => (current_index + 1) % count,
                    _ => (current_index + count - 1) % count,
                };
                snapshot.playback.track = Some(snapshot.library[next_index].clone());
                snapshot.playback.position_ms = 0;
                snapshot.playback.playing = true;
            }
        }
    }
    Ok(snapshot.clone())
}

#[tauri::command]
fn begin_login(state: State<'_, AppState>) -> Result<(), String> {
    if spotify_client_id().is_none() {
        return Err("Spotify OAuth is not configured. Set MAGNET_SPOTIFY_CLIENT_ID for the desktop build, then restart Magnet Player.".into());
    }
    let mut snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?;
    snapshot.message = Some("Spotify OAuth bridge is reserved for the librespot core; no credentials were sent from the renderer.".into());
    Err(
        "The app identity is present, but librespot OAuth is not linked in this shell build yet."
            .into(),
    )
}

#[tauri::command]
fn export_diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let path = root.join("magnet-player-diagnostics.json");
    let snapshot = state
        .snapshot
        .lock()
        .map_err(|_| "Player state is unavailable.".to_string())?
        .clone();
    let report = serde_json::json!({
        "generated_at_unix_ms": now_ms(),
        "app": "magnet-player",
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

fn spotify_client_id() -> Option<String> {
    option_env!("MAGNET_SPOTIFY_CLIENT_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn start_visual_emitter(app: AppHandle) {
    thread::spawn(move || loop {
        let now = now_ms() as f32;
        let pulse = ((now / 380.0).sin() + 1.0) * 0.5;
        let frame = VisualFrame {
            timestamp_ms: now as u128,
            bass: 0.26 + pulse * 0.54,
            mid: 0.18 + (((now / 640.0).sin() + 1.0) * 0.5) * 0.36,
            treble: 0.12 + (((now / 220.0).sin() + 1.0) * 0.5) * 0.30,
            energy: 0.28 + pulse * 0.38,
            onset: if pulse > 0.92 { 0.9 } else { 0.04 },
            stereo: (now / 900.0).sin() * 0.42,
            silence: false,
        };
        let _ = app.emit("visual-frame", frame);
        thread::sleep(Duration::from_millis(33));
    });
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            start_visual_emitter(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            preferences,
            set_preferences,
            dispatch,
            begin_login,
            export_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("error while running Magnet Player");
}

fn demo_library() -> Vec<Track> {
    [
        ("Fontaines D.C.", "Nabokov", "5:21"),
        (
            "You Don't Need Anyone",
            "oskar med k, kris., mondaé",
            "2:38",
        ),
        ("Flagstaff", "Ax and the Hatchetmen", "2:47"),
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
