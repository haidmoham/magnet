# Magnet Player

A Windows-first Spotify desktop client with a dense, keyboard-forward library UI and a restrained VoidPulse visual layer.

## Current alpha shell

The repository contains the packaged Tauri desktop shell, the WebGL visual layer, wallpaper-backed static visual end state, interaction model, adaptive quality controller, local preferences, diagnostics export, and a typed Rust/TypeScript bridge. Spotify credentials and the librespot/ncspot data core are deliberately isolated behind that bridge.

## Development

```powershell
npm install
npm run tauri dev
```

## Product constraints

- The existing VoidPulse website is not a dependency.
- Visual controls are intentionally limited to visuals, intensity, and quality.
- The physics-inspired visual layer is an intentional performance tradeoff: it
  must respond to decoded audio, retain a wallpaper-backed static end state,
  and never be used to excuse sluggish interaction.
- New product scope stays atomic and deliberate. Prefer a specific music-player
  capability that earns its place in the keyboard-forward flow over broad
  preference surfaces or feature accumulation.
- Playback must use a dedicated Spotify app identity and a Premium account.
- No analytics or remote telemetry are included.

## Third-party lineage

The production Spotify core will adapt selected components from [ncspot](https://github.com/hrkfdn/ncspot), which is BSD-2-Clause licensed. See `NOTICE` before importing or shipping adapted source.
