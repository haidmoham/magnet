import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppSnapshot, PlayerAction, Preferences, VisualFrame } from "./types";

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
  beginLogin: () => command<void>("begin_login"),
  exportDiagnostics: () => command<string>("export_diagnostics"),
  onVisualFrame: async (callback: (frame: VisualFrame) => void): Promise<UnlistenFn | null> =>
    isTauri ? listen<VisualFrame>("visual-frame", ({ payload }) => callback(payload)) : null,
};

