import type { AppSnapshot, Preferences, Track, VisualFrame } from "./types";

export const demoTracks: Track[] = [
  { id: "1", title: "Fontaines D.C.", artists: ["Nabokov"], album: "August 2026", durationMs: 321000, saved: true },
  { id: "2", title: "You Don't Need Anyone", artists: ["oskar med k", "kris.", "mondaé"], album: "August 2026", durationMs: 158000 },
  { id: "3", title: "Flagstaff", artists: ["Ax and the Hatchetmen"], album: "August 2026", durationMs: 167000 },
  { id: "4", title: "Cross The Street", artists: ["Junior Varsity"], album: "August 2026", durationMs: 167000 },
  { id: "5", title: "Truth", artists: ["Flycatcher"], album: "August 2026", durationMs: 192000 },
  { id: "6", title: "East Village", artists: ["Spacey Jane"], album: "August 2026", durationMs: 211000 },
  { id: "7", title: "Sunscreen", artists: ["Ax and the Hatchetmen"], album: "August 2026", durationMs: 174000 },
  { id: "8", title: "Great Pretender", artists: ["Dominic Fike"], album: "August 2026", durationMs: 171000 },
  { id: "9", title: "What Do We Ever Really Know?", artists: ["Balu Brigada"], album: "August 2026", durationMs: 233000 },
  { id: "10", title: "Warm Nights", artists: ["Royel Otis"], album: "August 2026", durationMs: 211000 },
  { id: "11", title: "SPEND THE WEEK", artists: ["Laszewo"], album: "August 2026", durationMs: 165000 },
  { id: "12", title: "Hotel Room", artists: ["Ax and the Hatchetmen"], album: "August 2026", durationMs: 148000 },
  { id: "13", title: "Golden Gate Girl", artists: ["Balu Brigada"], album: "August 2026", durationMs: 197000 },
  { id: "14", title: "Mother", artists: ["Royel Otis"], album: "August 2026", durationMs: 193000 },
  { id: "15", title: "New York", artists: ["Junior Varsity"], album: "August 2026", durationMs: 163000 },
  { id: "16", title: "Right As Rain", artists: ["MisterWives"], album: "August 2026", durationMs: 257000 },
];

export const demoPreferences: Preferences = {
  visualsEnabled: false,
  foregroundHidden: false,
  intensity: "standard",
  quality: "auto",
};

export const demoSnapshot: AppSnapshot = {
  revision: 0,
  view: "library",
  library: demoTracks,
  playlists: [],
  queue: demoTracks.slice(0, 6).map((track, index) => ({ queueId: `demo-${index + 1}`, track })),
  playback: {
    track: demoTracks[0],
    positionMs: 194000,
    playing: true,
    volume: 0.65,
    shuffle: false,
    repeat: "off",
  },
  authenticated: false,
  spotifyConfigured: false,
  message: "Desktop shell preview — Spotify connection requires your bundled app ID.",
};

export function demoVisualFrame(now: number): VisualFrame {
  const pulse = (Math.sin(now / 380) + 1) / 2;
  return {
    timestampMs: now,
    bass: 0.26 + pulse * 0.54,
    mid: 0.18 + ((Math.sin(now / 640) + 1) / 2) * 0.36,
    treble: 0.12 + ((Math.sin(now / 220) + 1) / 2) * 0.3,
    energy: 0.28 + pulse * 0.38,
    onset: pulse > 0.92 ? 0.9 : 0.04,
    stereo: Math.sin(now / 900) * 0.42,
    silence: false,
  };
}
