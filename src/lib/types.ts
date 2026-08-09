export type ViewId = "library" | "search" | "queue" | "browse";

export interface Track {
  id: string;
  title: string;
  artists: string[];
  album: string;
  durationMs: number;
  saved?: boolean;
}

export interface Playlist {
  id: string;
  name: string;
  owner: string;
  trackCount: number;
}

export interface QueueEntry {
  queueId: string;
  track: Track;
}

export type SearchKind = "tracks" | "playlists";

export interface SearchPage<T extends Track | Playlist> {
  requestId: string;
  items: T[];
  nextCursor: string | null;
}

export interface PlaybackState {
  track: Track | null;
  positionMs: number;
  playing: boolean;
  volume: number;
  shuffle: boolean;
  repeat: "off" | "context" | "track";
}

export interface AppSnapshot {
  revision: number;
  view: ViewId;
  library: Track[];
  playlists: Playlist[];
  queue: QueueEntry[];
  playback: PlaybackState;
  authenticated: boolean;
  catalogLoading: boolean;
  spotifyConfigured: boolean;
  message: string | null;
}

export interface VisualFrame {
  timestampMs: number;
  bass: number;
  mid: number;
  treble: number;
  energy: number;
  peak?: number;
  onset: number;
  spectrum?: number[];
  stereo: number;
  silence: boolean;
}

export interface Preferences {
  foregroundHidden: boolean;
}

export interface SpotifyAuthResult {
  authenticated: boolean;
  message: string;
}

export type PlayerAction =
  | { type: "play_track"; trackId: string; track?: Track }
  | { type: "play_playlist"; playlistId: string }
  | { type: "set_saved"; trackId: string; track?: Track; saved: boolean }
  | { type: "add_to_playlist"; trackId: string; playlistId: string }
  | { type: "toggle_playback" }
  | { type: "next" }
  | { type: "previous" }
  | { type: "seek"; positionMs: number }
  | { type: "set_volume"; volume: number }
  | { type: "toggle_shuffle" }
  | { type: "cycle_repeat" }
  | { type: "enqueue"; trackId: string; track?: Track }
  | { type: "move_queue_item"; queueId: string; toIndex: number }
  | { type: "remove_queue_item"; queueId: string }
  | { type: "clear_queue" }
  | { type: "set_view"; view: ViewId };
