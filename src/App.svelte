<script lang="ts">
  import { onMount } from "svelte";
  import { bridge } from "./lib/bridge";
  import { demoPreferences, demoSnapshot, demoVisualFrame } from "./lib/demo";
  import { duration, percent } from "./lib/format";
  import type { AppSnapshot, PlayerAction, Preferences, Track, ViewId, VisualIntensity, VisualQuality } from "./lib/types";

  let snapshot: AppSnapshot = demoSnapshot;
  let preferences: Preferences = demoPreferences;
  let selectedId = demoSnapshot.playback.track?.id ?? demoSnapshot.library[0]?.id ?? "";
  let query = "";
  let commandOpen = false;
  let commandText = "";
  let settingsOpen = false;
  let warningOpen = true;
  let rendererHost: HTMLDivElement;
  let renderer: import("./lib/visualizer").NebulaRenderer | null = null;
  let statusMessage = snapshot.message;

  $: trackRows = snapshot.view === "queue" ? snapshot.queue : snapshot.library;
  $: visibleTracks = query.trim()
    ? trackRows.filter((track) => `${track.title} ${track.artists.join(" ")} ${track.album}`.toLowerCase().includes(query.toLowerCase()))
    : trackRows;
  $: current = snapshot.playback.track;
  $: progress = current ? Math.min(1, snapshot.playback.positionMs / current.durationMs) : 0;

  function selectedIndex(): number {
    return Math.max(0, visibleTracks.findIndex((track) => track.id === selectedId));
  }

  async function refresh(): Promise<void> {
    if (!bridge.isTauri) return;
    try {
      snapshot = await bridge.snapshot();
      preferences = await bridge.preferences();
      renderer?.setPreferences(preferences);
      statusMessage = snapshot.message;
    } catch (error) {
      statusMessage = error instanceof Error ? error.message : String(error);
    }
  }

  async function dispatch(action: PlayerAction): Promise<void> {
    if (!bridge.isTauri) {
      snapshot = applyDemoAction(snapshot, action);
      selectedId = snapshot.playback.track?.id ?? selectedId;
      return;
    }
    try {
      snapshot = await bridge.dispatch(action);
      statusMessage = snapshot.message;
    } catch (error) {
      statusMessage = error instanceof Error ? error.message : String(error);
    }
  }

  function applyDemoAction(state: AppSnapshot, action: PlayerAction): AppSnapshot {
    if (action.type === "set_view") return { ...state, view: action.view };
    if (action.type === "play_track") {
      const track = state.library.find((item) => item.id === action.trackId) ?? null;
      return { ...state, playback: { ...state.playback, track, positionMs: 0, playing: true } };
    }
    if (action.type === "toggle_playback") return { ...state, playback: { ...state.playback, playing: !state.playback.playing } };
    if (action.type === "set_volume") return { ...state, playback: { ...state.playback, volume: action.volume } };
    if (action.type === "seek") return { ...state, playback: { ...state.playback, positionMs: action.positionMs } };
    if (action.type === "toggle_shuffle") return { ...state, playback: { ...state.playback, shuffle: !state.playback.shuffle } };
    if (action.type === "cycle_repeat") {
      const repeat = state.playback.repeat === "off" ? "context" : state.playback.repeat === "context" ? "track" : "off";
      return { ...state, playback: { ...state.playback, repeat } };
    }
    if (action.type === "next" || action.type === "previous") {
      const index = state.library.findIndex((item) => item.id === state.playback.track?.id);
      const step = action.type === "next" ? 1 : -1;
      const track = state.library[(index + step + state.library.length) % state.library.length];
      return { ...state, playback: { ...state.playback, track, positionMs: 0, playing: true } };
    }
    if (action.type === "enqueue") {
      const track = state.library.find((item) => item.id === action.trackId);
      return track ? { ...state, queue: [...state.queue, track] } : state;
    }
    return state;
  }

  async function setPreferences(next: Preferences): Promise<void> {
    preferences = next;
    renderer?.setPreferences(next);
    if (bridge.isTauri) {
      try {
        await bridge.setPreferences(next);
      } catch (error) {
        statusMessage = error instanceof Error ? error.message : String(error);
      }
    }
  }

  async function login(): Promise<void> {
    if (!bridge.isTauri) {
      statusMessage = "Run Magnet Player as a Tauri desktop app to begin Spotify OAuth.";
      return;
    }
    try {
      await bridge.beginLogin();
      statusMessage = "Opening Spotify sign-in in your browser…";
    } catch (error) {
      statusMessage = error instanceof Error ? error.message : String(error);
    }
  }

  async function exportDiagnostics(): Promise<void> {
    if (!bridge.isTauri) {
      statusMessage = "Diagnostics export is available in the packaged desktop app.";
      return;
    }
    try {
      const path = await bridge.exportDiagnostics();
      statusMessage = `Diagnostics exported to ${path}`;
    } catch (error) {
      statusMessage = error instanceof Error ? error.message : String(error);
    }
  }

  function activate(track: Track): void {
    selectedId = track.id;
    void dispatch({ type: "play_track", trackId: track.id });
  }

  function openView(view: ViewId): void {
    query = "";
    void dispatch({ type: "set_view", view });
  }

  function keydown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement;
    const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
    if (typing && event.key !== "Escape") return;

    if (event.key === "/") {
      event.preventDefault();
      const search = document.querySelector<HTMLInputElement>("#search");
      search?.focus();
      return;
    }
    if (event.key === ":") {
      event.preventDefault();
      commandOpen = true;
      return;
    }
    if (event.key === "Escape") {
      commandOpen = false;
      settingsOpen = false;
      query = "";
      return;
    }
    if (event.key === " ") {
      event.preventDefault();
      void dispatch({ type: "toggle_playback" });
      return;
    }
    if (event.key === "F1") { event.preventDefault(); openView("queue"); return; }
    if (event.key === "F2") { event.preventDefault(); openView("search"); return; }
    if (event.key === "F3") { event.preventDefault(); openView("library"); return; }
    if (event.key === "ArrowDown" || event.key.toLowerCase() === "j") {
      event.preventDefault();
      const next = visibleTracks[Math.min(selectedIndex() + 1, visibleTracks.length - 1)];
      if (next) selectedId = next.id;
      return;
    }
    if (event.key === "ArrowUp" || event.key.toLowerCase() === "k") {
      event.preventDefault();
      const previous = visibleTracks[Math.max(selectedIndex() - 1, 0)];
      if (previous) selectedId = previous.id;
      return;
    }
    if (event.key === "Enter") {
      const selected = visibleTracks[selectedIndex()];
      if (selected) activate(selected);
    }
  }

  function runCommand(): void {
    const value = commandText.trim().toLowerCase();
    if (value === "queue" || value === "q") openView("queue");
    if (value === "library" || value === "l") openView("library");
    if (value === "search" || value === "s") openView("search");
    if (value === "playpause" || value === "pause") void dispatch({ type: "toggle_playback" });
    if (value === "next") void dispatch({ type: "next" });
    if (value === "previous" || value === "prev") void dispatch({ type: "previous" });
    commandText = "";
    commandOpen = false;
  }

  onMount(() => {
    let stopDemo = 0;
    let unlisten: (() => void) | null = null;

    void (async () => {
      const { NebulaRenderer } = await import("./lib/visualizer");
      renderer = new NebulaRenderer(rendererHost);
      renderer.setPreferences(preferences);
      await refresh();
      unlisten = await bridge.onVisualFrame((frame) => renderer?.setFrame(frame));
      if (!bridge.isTauri) {
        const tick = () => {
          renderer?.setFrame(demoVisualFrame(performance.now()));
          stopDemo = requestAnimationFrame(tick);
        };
        stopDemo = requestAnimationFrame(tick);
      }
    })();

    return () => {
      cancelAnimationFrame(stopDemo);
      unlisten?.();
      renderer?.dispose();
    };
  });
</script>

<svelte:window on:keydown={keydown} />

<main class:visuals-off={!preferences.visualsEnabled}>
  <div class="nebula" bind:this={rendererHost} aria-hidden="true"></div>
  <div class="vignette" aria-hidden="true"></div>

  <section class="shell" aria-label="Magnet Player">
    <header class="titlebar" data-tauri-drag-region>
      <div class="brand" data-tauri-drag-region><span class="brand-mark">✦</span> magnet player</div>
      <div class="connection" data-tauri-drag-region>
        <span class:online={snapshot.authenticated} class="connection-dot"></span>
        {snapshot.authenticated ? "spotify connected" : "offline library"}
      </div>
      <button class="icon-button settings-button" aria-label="Open visual preferences" on:click={() => settingsOpen = !settingsOpen}>◌</button>
    </header>

    <div class="navigation">
      <button class:active={snapshot.view === "library"} on:click={() => openView("library")}>‹ library</button>
      <button class:active={snapshot.view === "search"} on:click={() => openView("search")}>search</button>
      <button class:active={snapshot.view === "queue"} on:click={() => openView("queue")}>queue <span>{snapshot.queue.length}</span></button>
      <button class:active={snapshot.view === "browse"} on:click={() => openView("browse")}>browse</button>
      <div class="nav-spacer"></div>
      <label class="search-field">
        <span>/</span>
        <input id="search" bind:value={query} placeholder="filter or search" aria-label="Filter current list" />
      </label>
    </div>

    <div class="view-title">
      <div>
        <span class="eyebrow">{snapshot.view}</span>
        <h1>{snapshot.view === "queue" ? "Playing queue" : snapshot.view === "search" ? "Search Spotify" : "Your library"}</h1>
      </div>
      <div class="view-meta">{visibleTracks.length} tracks · {duration(visibleTracks.reduce((sum, track) => sum + track.durationMs, 0))}</div>
    </div>

    <div class="track-list" role="listbox" aria-label="Tracks">
      {#each visibleTracks as track, index (track.id)}
        <button
          class:selected={track.id === selectedId}
          class:playing={track.id === current?.id}
          class="track-row"
          role="option"
          aria-selected={track.id === selectedId}
          on:click={() => selectedId = track.id}
          on:dblclick={() => activate(track)}
          on:contextmenu={(event) => { event.preventDefault(); void dispatch({ type: "enqueue", trackId: track.id }); statusMessage = `Queued ${track.title}`; }}
        >
          <span class="track-index">{track.id === current?.id && snapshot.playback.playing ? "▸" : String(index + 1).padStart(2, "0")}</span>
          <span class="track-main">
            <strong>{track.title}</strong>
            <span>{track.artists.join(", ")}</span>
          </span>
          <span class="track-album">{track.album}</span>
          <span class="track-duration">{duration(track.durationMs)}</span>
        </button>
      {:else}
        <div class="empty-state">no matching tracks</div>
      {/each}
    </div>

    <footer class="player">
      <div class="progress-row">
        <span>{current ? duration(snapshot.playback.positionMs) : "0:00"}</span>
        <input
          type="range"
          min="0"
          max={current?.durationMs ?? 1}
          value={snapshot.playback.positionMs}
          aria-label="Seek"
          on:input={(event) => void dispatch({ type: "seek", positionMs: Number(event.currentTarget.value) })}
        />
        <span>{current ? duration(current.durationMs) : "0:00"}</span>
      </div>
      <div class="now-playing">
        <div class="transport">
          <button class:active={snapshot.playback.shuffle} aria-label="Toggle shuffle" on:click={() => void dispatch({ type: "toggle_shuffle" })}>⤨</button>
          <button aria-label="Previous track" on:click={() => void dispatch({ type: "previous" })}>◀</button>
          <button class="play" aria-label="Toggle playback" on:click={() => void dispatch({ type: "toggle_playback" })}>{snapshot.playback.playing ? "Ⅱ" : "▶"}</button>
          <button aria-label="Next track" on:click={() => void dispatch({ type: "next" })}>▶</button>
          <button class:active={snapshot.playback.repeat !== "off"} aria-label="Cycle repeat" on:click={() => void dispatch({ type: "cycle_repeat" })}>↻</button>
        </div>
        <div class="track-summary">
          <strong>{current?.title ?? "Nothing playing"}</strong>
          <span>{current?.artists.join(", ") ?? "Connect Spotify to begin"}</span>
        </div>
        <label class="volume">
          <span>vol</span>
          <input type="range" min="0" max="1" step="0.01" value={snapshot.playback.volume} aria-label="Volume" on:input={(event) => void dispatch({ type: "set_volume", volume: Number(event.currentTarget.value) })} />
          <span>{percent(snapshot.playback.volume)}</span>
        </label>
      </div>
    </footer>
  </section>

  {#if settingsOpen}
    <aside class="settings" aria-label="Visual preferences">
      <div class="settings-title">visual system <button aria-label="Close preferences" on:click={() => settingsOpen = false}>×</button></div>
      <label class="toggle-row"><span>visuals</span><input type="checkbox" checked={preferences.visualsEnabled} on:change={(event) => void setPreferences({ ...preferences, visualsEnabled: event.currentTarget.checked })} /></label>
      <fieldset>
        <legend>intensity</legend>
        {#each ["calm", "standard", "high"] as intensity}
          <button class:active={preferences.intensity === intensity} on:click={() => void setPreferences({ ...preferences, intensity: intensity as VisualIntensity })}>{intensity}</button>
        {/each}
      </fieldset>
      <fieldset>
        <legend>quality</legend>
        {#each ["auto", "eco", "high"] as quality}
          <button class:active={preferences.quality === quality} on:click={() => void setPreferences({ ...preferences, quality: quality as VisualQuality })}>{quality}</button>
        {/each}
      </fieldset>
      <p>Visuals use live audio features and adapt their workload. There are no manual palettes, shapes, or shader controls.</p>
      <button class="diagnostics" on:click={exportDiagnostics}>export diagnostics</button>
    </aside>
  {/if}

  {#if !snapshot.authenticated}
    <button class="login-cta" on:click={login}>connect spotify <span>↗</span></button>
  {/if}

  {#if statusMessage}
    <div class="status-message" role="status">{statusMessage}</div>
  {/if}

  {#if commandOpen}
    <form class="command-palette" on:submit={(event) => { event.preventDefault(); runCommand(); }}>
      <span>:</span>
      <input bind:value={commandText} placeholder="queue · library · search · playpause · next" aria-label="Command" />
    </form>
  {/if}

  {#if warningOpen}
    <div class="warning" role="alertdialog" aria-modal="true" aria-labelledby="warning-title">
      <div>
        <span class="warning-mark">⚠</span>
        <p class="eyebrow">photosensitivity notice</p>
        <h2 id="warning-title">this player uses reactive, moving light.</h2>
        <p>Magnet Player’s visual layer responds to the music with particles, color transitions, and occasional bright pulses. You can disable visuals at any time.</p>
        <div class="warning-actions">
          <button class="ghost" on:click={() => { void setPreferences({ ...preferences, visualsEnabled: false }); warningOpen = false; }}>disable visuals</button>
          <button class="primary" on:click={() => warningOpen = false}>continue</button>
        </div>
      </div>
    </div>
  {/if}
</main>
