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
- Playback must use a dedicated Spotify app identity and a Premium account.
- No analytics or remote telemetry are included.

## Third-party lineage

The production Spotify core will adapt selected components from [ncspot](https://github.com/hrkfdn/ncspot), which is BSD-2-Clause licensed. See `NOTICE` before importing or shipping adapted source.
