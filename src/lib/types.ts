export type ViewId = "library" | "search" | "queue" | "browse";
export type VisualIntensity = "calm" | "standard" | "high";
export type VisualQuality = "auto" | "eco" | "high";

export interface Track {
  id: string;
  title: string;
  artists: string[];
  album: string;
  durationMs: number;
  saved?: boolean;
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
  view: ViewId;
  library: Track[];
  queue: Track[];
  playback: PlaybackState;
  authenticated: boolean;
  spotifyConfigured: boolean;
  message: string | null;
}

export interface VisualFrame {
  timestampMs: number;
  bass: number;
  mid: number;
  treble: number;
  energy: number;
  onset: number;
  stereo: number;
  silence: boolean;
}

export interface Preferences {
  visualsEnabled: boolean;
  intensity: VisualIntensity;
  quality: VisualQuality;
}

export type PlayerAction =
  | { type: "play_track"; trackId: string }
  | { type: "toggle_playback" }
  | { type: "next" }
  | { type: "previous" }
  | { type: "seek"; positionMs: number }
  | { type: "set_volume"; volume: number }
  | { type: "toggle_shuffle" }
  | { type: "cycle_repeat" }
  | { type: "enqueue"; trackId: string }
  | { type: "set_view"; view: ViewId };

