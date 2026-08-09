import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppSnapshot, PlayerAction, Playlist, Preferences, SearchKind, SearchPage, SpotifyAuthResult, Track, VisualFrame } from "./types";

const isTauri = "__TAURI_INTERNALS__" in window;

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) throw new Error("This action is available in the desktop app only.");
  return invoke<T>(name, args);
}

export const bridge = {
  isTauri,
  snapshot: () => command<AppSnapshot>("snapshot"),
  preferences: () => command<Preferences>("preferences"),
  setPreferences: (preferences: Preferences) => command<void>("set_preferences", { preferences }),
  dispatch: (action: PlayerAction) => command<AppSnapshot>("dispatch", { action }),
  searchSpotify: <T extends Track | Playlist>(query: string, kind: SearchKind, cursor?: string) =>
    command<SearchPage<T>>("search_spotify", { query, kind, cursor }),
  playlistTracks: (playlistId: string) => command<Track[]>("playlist_tracks", { playlistId }),
  beginLogin: () => command<void>("begin_login"),
  exportDiagnostics: () => command<string>("export_diagnostics"),
  onSpotifyAuth: async (callback: (result: SpotifyAuthResult) => void): Promise<UnlistenFn | null> =>
    isTauri ? listen<SpotifyAuthResult>("spotify-auth-complete", ({ payload }) => callback(payload)) : null,
  onPlayerState: async (callback: (state: AppSnapshot) => void): Promise<UnlistenFn | null> =>
    isTauri ? listen<AppSnapshot>("player-state", ({ payload }) => callback(payload)) : null,
  onVisualFrame: async (callback: (frame: VisualFrame) => void): Promise<UnlistenFn | null> =>
    isTauri ? listen<VisualFrame>("visual-frame", ({ payload }) => callback(payload)) : null,
};
