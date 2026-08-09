<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { bridge } from "./lib/bridge";
  import { demoPreferences, demoSnapshot, demoVisualFrame } from "./lib/demo";
  import { duration, percent } from "./lib/format";
  import type { AppSnapshot, PlayerAction, Playlist, Preferences, SearchKind, Track, ViewId, VisualFrame } from "./lib/types";

  let snapshot: AppSnapshot = demoSnapshot;
  let preferences: Preferences = demoPreferences;
  let selectedId = demoSnapshot.playback.track?.id ?? demoSnapshot.library[0]?.id ?? "";
  let query = "";
  let searchKind: SearchKind = "tracks";
  let searchTracks: Track[] = [];
  let searchPlaylists: Playlist[] = [];
  let searchCursors: Record<SearchKind, string | null> = { tracks: null, playlists: null };
  let searchPending = false;
  let searchError: string | null = null;
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  let searchRevision = 0;
  let openedPlaylist: Playlist | null = null;
  let openedPlaylistTracks: Track[] = [];
  let playlistOpening = false;
  let playlistOpenError: string | null = null;
  let playlistOpenTimer: ReturnType<typeof setTimeout> | null = null;
  let selectedQueueId = demoSnapshot.queue[0]?.queueId ?? "";
  let draggedQueueId = "";
  let commandOpen = false;
  let commandText = "";
  let settingsOpen = false;
  let trackMenu: TrackMenu | null = null;
  let playlistMenu: PlaylistMenu | null = null;
  let selectedArtworkId = "nebula";
  let customArtworkUrl: string | null = null;
  let statusMessage = snapshot.message;
  let visualFrame: VisualFrame = demoVisualFrame(0);
  let interactionRevision = 0;
  let volumeCommit: ReturnType<typeof setTimeout> | null = null;
  let seeking = false;

  $: playlistDetailActive = snapshot.view === "browse" && openedPlaylist !== null;
  $: showingPlaylists = !playlistDetailActive && (snapshot.view === "browse" || (snapshot.view === "search" && searchKind === "playlists"));
  $: trackRows = !snapshot.authenticated
    ? snapshot.library.slice(0, 3)
    : snapshot.view === "queue" ? snapshot.queue.map((entry) => entry.track) : snapshot.view === "search" ? searchTracks : playlistDetailActive ? openedPlaylistTracks : snapshot.view === "browse" ? [] : snapshot.library;
  $: visibleTracks = snapshot.view !== "search" && query.trim()
    ? trackRows.filter((track) => `${track.title} ${track.artists.join(" ")} ${track.album}`.toLowerCase().includes(query.toLowerCase()))
    : trackRows;
  $: visiblePlaylists = snapshot.authenticated ? (snapshot.view === "search" ? searchPlaylists : snapshot.playlists) : [];
  $: current = snapshot.playback.track;
  $: progress = current ? Math.min(1, snapshot.playback.positionMs / current.durationMs) : 0;
  $: spectrumBars = makeSpectrumBars(visualFrame);
  $: activeArtwork = customArtworkUrl ?? spaceArtworks.find((artwork) => artwork.id === selectedArtworkId)?.src ?? spaceArtworks[0].src;
  $: if (snapshot.view === "search") scheduleSpotifySearch(query, searchKind);

  function makeSpectrumBars(frame: VisualFrame): number[] {
    if (frame.spectrum?.length) {
      return Array.from({ length: 48 }, (_, index) => {
        const idleContour = 0.035 + (Math.sin(index * 1.73) + 1) * 0.012;
        return Math.max(idleContour, Math.min(1, frame.spectrum?.[index] ?? 0));
      });
    }
    return Array.from({ length: 48 }, (_, index) => {
      const position = index / 47;
      const band = position < 0.3 ? frame.bass : position < 0.7 ? frame.mid : frame.treble;
      const idleContour = 0.035 + (Math.sin(index * 1.73) + 1) * 0.012;
      return Math.max(idleContour, Math.min(1, band + frame.energy * 0.12));
    });
  }

  const spaceArtworks = [
    { id: "nebula", name: "Violet Field", credit: "Magnet", src: new URL("./assets/nebula-static.webp", import.meta.url).href },
    { id: "carina", name: "Carina Nebula", credit: "NASA / ESA / Hubble", src: new URL("./assets/space/carina.webp", import.meta.url).href },
    { id: "ngc1300", name: "NGC 1300", credit: "NASA / ESA / Hubble", src: new URL("./assets/space/ngc1300.webp", import.meta.url).href },
    { id: "helix", name: "Helix Nebula", credit: "NASA / NOAO / ESA / Hubble", src: new URL("./assets/space/helix.webp", import.meta.url).href },
  ];

  type TrackMenu = { track: Track; x: number; y: number; playlistPicker: boolean };
  type PlaylistMenu = { playlist: Playlist; x: number; y: number };

  function selectedIndex(): number {
    return Math.max(0, visibleTracks.findIndex((track) => track.id === selectedId));
  }

  function selectedQueueIndex(): number {
    return Math.max(0, snapshot.queue.findIndex((entry) => entry.queueId === selectedQueueId));
  }

  function scheduleSpotifySearch(value: string, kind: SearchKind): void {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = null;
    const normalized = value.trim();
    const revision = ++searchRevision;
    if (!normalized) {
      searchTracks = [];
      searchPlaylists = [];
      searchCursors = { tracks: null, playlists: null };
      searchPending = false;
      searchError = null;
      return;
    }
    searchTimer = setTimeout(() => {
      searchTimer = null;
      void searchSpotify(normalized, kind, false, revision);
    }, 200);
  }

  async function searchSpotify(value: string, kind: SearchKind, append: boolean, requestedRevision = searchRevision): Promise<void> {
    const revision = append ? searchRevision : requestedRevision;
    searchPending = true;
    searchError = null;
    try {
      if (!bridge.isTauri) {
        const normalized = value.toLowerCase();
        if (kind === "tracks") searchTracks = demoSnapshot.library.filter((track) => `${track.title} ${track.artists.join(" ")} ${track.album}`.toLowerCase().includes(normalized));
        else searchPlaylists = [];
        searchCursors = { ...searchCursors, [kind]: null };
        return;
      }
      const page = kind === "tracks"
        ? await bridge.searchSpotify<Track>(value, kind, append ? searchCursors[kind] ?? undefined : undefined)
        : await bridge.searchSpotify<Playlist>(value, kind, append ? searchCursors[kind] ?? undefined : undefined);
      if (revision !== searchRevision || value !== query.trim() || kind !== searchKind) return;
      if (kind === "tracks") searchTracks = mergeSearchItems(searchTracks, page.items as Track[], append);
      else searchPlaylists = mergeSearchItems(searchPlaylists, page.items as Playlist[], append);
      searchCursors = { ...searchCursors, [kind]: page.nextCursor };
    } catch (error) {
      if (revision === searchRevision) searchError = error instanceof Error ? error.message : String(error);
    } finally {
      if (revision === searchRevision) searchPending = false;
    }
  }

  function mergeSearchItems<T extends { id: string }>(existing: T[], incoming: T[], append: boolean): T[] {
    if (!append) return incoming;
    const merged = new Map(existing.map((item) => [item.id, item]));
    for (const item of incoming) merged.set(item.id, item);
    return [...merged.values()];
  }

  async function refresh(): Promise<void> {
    if (!bridge.isTauri) return;
    try {
      snapshot = await bridge.snapshot();
      preferences = await bridge.preferences();
      statusMessage = snapshot.message;
    } catch (error) {
      statusMessage = error instanceof Error ? error.message : String(error);
    }
  }

  function interactionMessage(action: PlayerAction, next: AppSnapshot): string | null {
    if (action.type === "toggle_playback") return next.playback.playing ? "Resuming…" : "Pausing…";
    if (action.type === "next") return "Next track…";
    if (action.type === "previous") return "Previous track…";
    if (action.type === "play_track") return next.playback.track ? `Playing ${next.playback.track.title}…` : "Loading track…";
    if (action.type === "play_playlist") return "Starting playlist…";
    return null;
  }

  async function dispatch(action: PlayerAction, options: { optimistic?: boolean; quiet?: boolean } = {}): Promise<void> {
    if (!bridge.isTauri) {
      snapshot = applyDemoAction(snapshot, action);
      selectedId = snapshot.playback.track?.id ?? selectedId;
      return;
    }
    const revision = ++interactionRevision;
    const previous = snapshot;
    const optimistic = options.optimistic !== false;
    if (optimistic) {
      snapshot = applyDemoAction(snapshot, action);
      selectedId = snapshot.playback.track?.id ?? selectedId;
      if (!options.quiet) statusMessage = interactionMessage(action, snapshot) ?? statusMessage;
    }
    try {
      const confirmed = await bridge.dispatch(action);
      if (revision === interactionRevision) {
        snapshot = confirmed;
        if (!options.quiet && confirmed.message) statusMessage = confirmed.message;
      }
    } catch (error) {
      if (revision === interactionRevision) {
        snapshot = previous;
        statusMessage = error instanceof Error ? error.message : String(error);
      }
    }
  }

  function previewSeek(positionMs: number): void {
    seeking = true;
    snapshot = { ...snapshot, playback: { ...snapshot.playback, positionMs } };
  }

  function commitSeek(positionMs: number): void {
    seeking = false;
    void dispatch({ type: "seek", positionMs }, { optimistic: false, quiet: true });
  }

  function previewVolume(volume: number): void {
    snapshot = { ...snapshot, playback: { ...snapshot.playback, volume } };
    if (volumeCommit) clearTimeout(volumeCommit);
    volumeCommit = setTimeout(() => {
      volumeCommit = null;
      void dispatch({ type: "set_volume", volume }, { optimistic: false, quiet: true });
    }, 55);
  }

  function commitVolume(volume: number): void {
    if (volumeCommit) clearTimeout(volumeCommit);
    volumeCommit = null;
    void dispatch({ type: "set_volume", volume }, { optimistic: false, quiet: true });
  }

  function applyDemoAction(state: AppSnapshot, action: PlayerAction): AppSnapshot {
    if (action.type === "set_view") return { ...state, view: action.view };
    if (action.type === "play_track") {
      const track = state.library.find((item) => item.id === action.trackId) ?? null;
      return { ...state, playback: { ...state.playback, track, positionMs: 0, playing: true } };
    }
    if (action.type === "play_playlist") return state;
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
      return track ? { ...state, queue: [...state.queue, { queueId: `demo-${Date.now()}-${state.queue.length}`, track }] } : state;
    }
    if (action.type === "move_queue_item") {
      const fromIndex = state.queue.findIndex((entry) => entry.queueId === action.queueId);
      if (fromIndex < 0 || action.toIndex < 0 || action.toIndex >= state.queue.length) return state;
      const queue = [...state.queue];
      const [entry] = queue.splice(fromIndex, 1);
      queue.splice(action.toIndex, 0, entry);
      return { ...state, queue };
    }
    if (action.type === "remove_queue_item") {
      return { ...state, queue: state.queue.filter((entry) => entry.queueId !== action.queueId) };
    }
    if (action.type === "clear_queue") return { ...state, queue: [] };
    return state;
  }

  async function setPreferences(next: Preferences): Promise<void> {
    preferences = next;
    if (bridge.isTauri) {
      try {
        await bridge.setPreferences(next);
      } catch (error) {
        statusMessage = error instanceof Error ? error.message : String(error);
      }
    }
  }

  function enterVisualMode(): void {
    settingsOpen = false;
    commandOpen = false;
    void setPreferences({ ...preferences, foregroundHidden: true });
  }

  function exitVisualMode(): void {
    void setPreferences({ ...preferences, foregroundHidden: false });
  }

  function selectArtwork(id: string): void {
    selectedArtworkId = id;
    if (customArtworkUrl) {
      URL.revokeObjectURL(customArtworkUrl);
      customArtworkUrl = null;
    }
    void setPreferences(preferences);
  }

  function uploadArtwork(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      statusMessage = "Choose an image file (JPG, PNG, WebP, or AVIF).";
      return;
    }
    if (file.size > 16 * 1024 * 1024) {
      statusMessage = "Choose an image smaller than 16 MB.";
      return;
    }
    if (customArtworkUrl) URL.revokeObjectURL(customArtworkUrl);
    customArtworkUrl = URL.createObjectURL(file);
    selectedArtworkId = "custom";
    statusMessage = `Using ${file.name} as static artwork.`;
    void setPreferences(preferences);
  }

  async function login(): Promise<void> {
    if (!bridge.isTauri) {
      statusMessage = "Run Magnet as a Tauri desktop app to begin Spotify OAuth.";
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
    void dispatch({ type: "play_track", trackId: track.id, track });
  }

  function activatePlaylist(playlist: Playlist): void {
    void dispatch({ type: "play_playlist", playlistId: playlist.id });
    statusMessage = `Starting ${playlist.name}`;
  }

  function openTrackMenu(event: MouseEvent, track: Track): void {
    event.preventDefault();
    selectedId = track.id;
    playlistMenu = null;
    trackMenu = { track, x: Math.min(event.clientX, window.innerWidth - 250), y: Math.min(event.clientY, window.innerHeight - 360), playlistPicker: false };
  }

  function openPlaylistMenu(event: MouseEvent, playlist: Playlist): void {
    event.preventDefault();
    trackMenu = null;
    playlistMenu = { playlist, x: Math.min(event.clientX, window.innerWidth - 250), y: Math.min(event.clientY, window.innerHeight - 220) };
  }

  function openSelectedTrackMenu(): void {
    const track = snapshot.view === "queue" ? snapshot.queue[selectedQueueIndex()]?.track : visibleTracks[selectedIndex()];
    if (!track) return;
    playlistMenu = null;
    trackMenu = { track, x: Math.max(24, Math.round(window.innerWidth / 2 - 114)), y: Math.max(24, Math.round(window.innerHeight / 2 - 180)), playlistPicker: false };
  }

  function searchFromMenu(value: string): void {
    trackMenu = null;
    playlistMenu = null;
    searchKind = "tracks";
    openView("search");
    query = value;
  }

  async function queueNext(track: Track): Promise<void> {
    await dispatch({ type: "enqueue", trackId: track.id, track });
    const queued = snapshot.queue.at(-1);
    if (queued) await dispatch({ type: "move_queue_item", queueId: queued.queueId, toIndex: 0 });
    statusMessage = `${track.title} will play next.`;
  }

  async function copySpotifyLink(kind: "track" | "playlist", id: string): Promise<void> {
    const url = `https://open.spotify.com/${kind}/${id}`;
    try {
      await navigator.clipboard.writeText(url);
      statusMessage = "Spotify link copied.";
    } catch {
      statusMessage = url;
    }
  }

  function schedulePlaylistOpen(playlist: Playlist): void {
    if (playlistOpenTimer) clearTimeout(playlistOpenTimer);
    playlistOpenTimer = setTimeout(() => {
      playlistOpenTimer = null;
      void openPlaylist(playlist);
    }, 180);
  }

  async function openPlaylist(playlist: Playlist): Promise<void> {
    playlistOpening = true;
    playlistOpenError = null;
    openedPlaylist = playlist;
    openedPlaylistTracks = [];
    query = "";
    try {
      if (!bridge.isTauri) {
        openedPlaylistTracks = demoSnapshot.library;
        return;
      }
      openedPlaylistTracks = await bridge.playlistTracks(playlist.id);
    } catch (error) {
      playlistOpenError = error instanceof Error ? error.message : String(error);
    } finally {
      playlistOpening = false;
    }
  }

  function closePlaylist(): void {
    if (playlistOpenTimer) clearTimeout(playlistOpenTimer);
    playlistOpenTimer = null;
    openedPlaylist = null;
    openedPlaylistTracks = [];
    playlistOpenError = null;
    query = "";
  }

  function moveQueueItem(queueId: string, toIndex: number): void {
    if (toIndex < 0 || toIndex >= snapshot.queue.length) return;
    selectedQueueId = queueId;
    void dispatch({ type: "move_queue_item", queueId, toIndex });
  }

  function removeQueueItem(queueId: string): void {
    const index = snapshot.queue.findIndex((entry) => entry.queueId === queueId);
    const fallback = snapshot.queue[index + 1] ?? snapshot.queue[index - 1];
    selectedQueueId = fallback?.queueId ?? "";
    void dispatch({ type: "remove_queue_item", queueId });
  }

  function dropQueueItem(event: DragEvent, toIndex: number): void {
    event.preventDefault();
    const queueId = event.dataTransfer?.getData("text/plain") || draggedQueueId;
    draggedQueueId = "";
    if (queueId) moveQueueItem(queueId, toIndex);
  }

  function openView(view: ViewId): void {
    closePlaylist();
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
      trackMenu = null;
      playlistMenu = null;
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
    if (event.key === ".") {
      const track = snapshot.view === "queue" ? snapshot.queue[selectedQueueIndex()]?.track : visibleTracks[selectedIndex()];
      if (track) void queueNext(track);
      return;
    }
    if (event.key.toLowerCase() === "o") {
      openSelectedTrackMenu();
      return;
    }
    if (event.key.toLowerCase() === "s") {
      const track = snapshot.view === "queue" ? snapshot.queue[selectedQueueIndex()]?.track : visibleTracks[selectedIndex()];
      if (track) void dispatch({ type: "set_saved", trackId: track.id, track, saved: !track.saved });
      return;
    }
    if (event.key.toLowerCase() === "x") {
      const track = snapshot.view === "queue" ? snapshot.queue[selectedQueueIndex()]?.track : visibleTracks[selectedIndex()];
      if (track) void copySpotifyLink("track", track.id);
      return;
    }
    if (event.key === "F1") { event.preventDefault(); openView("queue"); return; }
    if (event.key === "F2") { event.preventDefault(); openView("search"); return; }
    if (event.key === "F3") { event.preventDefault(); openView("library"); return; }
    if (snapshot.view === "queue" && event.altKey && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const index = selectedQueueIndex();
      moveQueueItem(selectedQueueId, event.key === "ArrowDown" ? index + 1 : index - 1);
      return;
    }
    if (snapshot.view === "queue" && event.key === "Delete") {
      event.preventDefault();
      if (selectedQueueId) removeQueueItem(selectedQueueId);
      return;
    }
    if (event.key === "ArrowDown" || event.key.toLowerCase() === "j") {
      event.preventDefault();
      if (snapshot.view === "queue") {
        const next = snapshot.queue[Math.min(selectedQueueIndex() + 1, snapshot.queue.length - 1)];
        if (next) { selectedQueueId = next.queueId; selectedId = next.track.id; }
      } else {
        const next = visibleTracks[Math.min(selectedIndex() + 1, visibleTracks.length - 1)];
        if (next) selectedId = next.id;
      }
      return;
    }
    if (event.key === "ArrowUp" || event.key.toLowerCase() === "k") {
      event.preventDefault();
      if (snapshot.view === "queue") {
        const previous = snapshot.queue[Math.max(selectedQueueIndex() - 1, 0)];
        if (previous) { selectedQueueId = previous.queueId; selectedId = previous.track.id; }
      } else {
        const previous = visibleTracks[Math.max(selectedIndex() - 1, 0)];
        if (previous) selectedId = previous.id;
      }
      return;
    }
    if (event.key === "Enter") {
      const selected = snapshot.view === "queue" ? snapshot.queue[selectedQueueIndex()]?.track : visibleTracks[selectedIndex()];
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
    const playheadTimer = window.setInterval(() => {
      const track = snapshot.playback.track;
      if (!bridge.isTauri && !seeking && track && snapshot.playback.playing) {
        snapshot = {
          ...snapshot,
          playback: {
            ...snapshot.playback,
            positionMs: Math.min(track.durationMs, snapshot.playback.positionMs + 250),
          },
        };
      }
    }, 250);
    let unlisten: (() => void) | null = null;
    let unlistenAuth: (() => void) | null = null;
    let unlistenPlayer: (() => void) | null = null;

    void (async () => {
      await refresh();
      unlisten = await bridge.onVisualFrame((frame) => {
        visualFrame = frame;
      });
      unlistenAuth = await bridge.onSpotifyAuth((result) => {
        statusMessage = result.message;
        void refresh();
      });
      unlistenPlayer = await bridge.onPlayerState((next) => {
        if (next.revision >= snapshot.revision) {
          snapshot = next;
          selectedId = next.playback.track?.id ?? selectedId;
          if (!seeking && next.message) statusMessage = next.message;
        }
      });
      if (!bridge.isTauri) {
        const tick = () => {
          visualFrame = demoVisualFrame(performance.now());
          stopDemo = requestAnimationFrame(tick);
        };
        stopDemo = requestAnimationFrame(tick);
      }
    })();

    return () => {
      cancelAnimationFrame(stopDemo);
      window.clearInterval(playheadTimer);
      if (volumeCommit) clearTimeout(volumeCommit);
      if (searchTimer) clearTimeout(searchTimer);
      unlisten?.();
      unlistenAuth?.();
      unlistenPlayer?.();
    };
  });

  onDestroy(() => {
    if (customArtworkUrl) URL.revokeObjectURL(customArtworkUrl);
  });
</script>

<svelte:window on:keydown={keydown} />

<main class:foreground-hidden={preferences.foregroundHidden} style={`--static-art: url("${activeArtwork}")`}>
  <div class="nebula" aria-hidden="true"></div>
  <div class="vignette" aria-hidden="true"></div>
  <div class="frequency-overlay" role="img" aria-label="Frequency histogram">
    <div class="frequency-header"><span class="frequency-label">live spectrum</span><span>audio analysis</span></div>
    <div class="frequency-bars" aria-hidden="true">
      {#each spectrumBars as bar, index}
        <span class:wide={index % 7 === 0} class:thin={index % 5 === 0} class="frequency-bar" style={`--bar-height: ${Math.round(bar * 100)}%`}></span>
      {/each}
    </div>
  </div>

  <section class="shell" aria-label="Magnet">
    <header class="titlebar" data-tauri-drag-region>
      <div class="brand" data-tauri-drag-region><span class="brand-mark">✦</span> magnet</div>
      <div class="connection" data-tauri-drag-region>
        <span class:online={snapshot.authenticated} class="connection-dot"></span>
        {snapshot.authenticated ? "spotify connected" : "spotify not connected"}
      </div>
      <button class="visual-mode-button" aria-label="Enter visual mode" on:click={enterVisualMode}>focus visual</button>
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
        <input id="search" bind:value={query} placeholder={snapshot.view === "search" ? "search spotify" : "filter current list"} aria-label={snapshot.view === "search" ? "Search Spotify" : "Filter current list"} />
      </label>
    </div>

    <div class="view-title">
      <div>
        <span class="eyebrow">{snapshot.view}</span>
        <h1>{snapshot.view === "queue" ? "Playing queue" : snapshot.view === "search" ? "Search Spotify" : playlistDetailActive ? openedPlaylist?.name : snapshot.view === "browse" ? "Your playlists" : "Your library"}</h1>
      </div>
      <div class="view-actions">
        {#if snapshot.view === "search"}
          <div class="search-kinds" aria-label="Search type">
            <button class:active={searchKind === "tracks"} on:click={() => searchKind = "tracks"}>tracks</button>
            <button class:active={searchKind === "playlists"} on:click={() => searchKind = "playlists"}>playlists</button>
          </div>
        {:else if snapshot.view === "queue" && snapshot.queue.length}
          <button class="clear-queue" on:click={() => void dispatch({ type: "clear_queue" })}>clear</button>
        {:else if playlistDetailActive}
          <button class="clear-queue" on:click={closePlaylist}>‹ playlists</button>
        {/if}
        <div class="view-meta">{showingPlaylists ? `${visiblePlaylists.length} playlists` : `${visibleTracks.length} tracks · ${duration(visibleTracks.reduce((sum, track) => sum + track.durationMs, 0))}`}</div>
      </div>
    </div>

    <div class="track-list" role="listbox" aria-label={showingPlaylists ? "Playlists" : "Tracks"}>
      {#if !snapshot.authenticated}
        <section class="connect-stage" aria-label="Connect Spotify">
          <p class="eyebrow">spotify library</p>
          <h2>Connect your library.</h2>
          <p>Saved tracks and playlists will appear here.</p>
          <button on:click={login}>connect spotify</button>
          <div class="connect-preview" aria-label="Sample tracks">
            <div><span>01</span><strong>Nabokov</strong><em>Fontaines D.C.</em></div>
            <div><span>02</span><strong>This Modern Love</strong><em>Bloc Party</em></div>
            <div><span>03</span><strong>You Don't Need Anyone</strong><em>oskar med k, kris., mondaé</em></div>
          </div>
        </section>
      {:else if snapshot.view === "search" && !query.trim()}
        <div class="empty-state">type to search spotify</div>
      {:else if showingPlaylists}
        {#each visiblePlaylists as playlist, index (playlist.id)}
          <button
            class="track-row playlist-row"
            role="option"
            aria-selected="false"
            on:click={() => schedulePlaylistOpen(playlist)}
            on:dblclick={(event) => { event.preventDefault(); if (playlistOpenTimer) clearTimeout(playlistOpenTimer); playlistOpenTimer = null; activatePlaylist(playlist); }}
            on:contextmenu={(event) => openPlaylistMenu(event, playlist)}
          >
            <span class="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span class="track-main">
              <strong>{playlist.name}</strong>
              <span>{playlist.owner}</span>
            </span>
            <span class="track-album">playlist</span>
            <span class="track-duration">{playlist.trackCount} tracks</span>
          </button>
        {:else}
          <div class="empty-state">{searchPending ? "searching…" : searchError ?? "no playlists found"}</div>
        {/each}
        {#if searchCursors[searchKind] && !searchPending}
          <button class="load-more" on:click={() => void searchSpotify(query.trim(), searchKind, true)}>more results</button>
        {/if}
      {:else if snapshot.view === "queue"}
        {#each snapshot.queue as entry, index (entry.queueId)}
          <div
            class:selected={entry.queueId === selectedQueueId}
            class:playing={entry.track.id === current?.id}
            class="track-row queue-row"
            role="option"
            aria-selected={entry.queueId === selectedQueueId}
            tabindex={entry.queueId === selectedQueueId ? 0 : -1}
            draggable="true"
            on:click={() => { selectedQueueId = entry.queueId; selectedId = entry.track.id; }}
            on:keydown={(event) => { if (event.key === "Enter") activate(entry.track); }}
            on:dblclick={() => activate(entry.track)}
            on:dragstart={(event) => { draggedQueueId = entry.queueId; event.dataTransfer?.setData("text/plain", entry.queueId); }}
            on:dragover={(event) => event.preventDefault()}
            on:drop={(event) => dropQueueItem(event, index)}
          >
            <span class="queue-grip" aria-hidden="true">⠿</span>
            <span class="track-index">{String(index + 1).padStart(2, "0")}</span>
            <span class="track-main">
              <strong>{entry.track.title}</strong>
              <span>{entry.track.artists.join(", ")}</span>
            </span>
            <span class="track-album">{entry.track.album}</span>
            <span class="track-duration">{duration(entry.track.durationMs)}</span>
            <button class="queue-remove" aria-label={`Remove ${entry.track.title} from queue`} on:click={(event) => { event.stopPropagation(); removeQueueItem(entry.queueId); }}>×</button>
          </div>
        {:else}
          <div class="empty-state">queue is empty</div>
        {/each}
      {:else}
      {#each visibleTracks as track, index (track.id)}
        <button
          class:selected={track.id === selectedId}
          class:playing={track.id === current?.id}
          class="track-row"
          role="option"
          aria-selected={track.id === selectedId}
          on:click={() => selectedId = track.id}
          on:dblclick={() => activate(track)}
          on:contextmenu={(event) => openTrackMenu(event, track)}
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
        <div class="empty-state">{playlistDetailActive ? (playlistOpening ? "opening playlist…" : playlistOpenError ?? "this playlist is empty") : snapshot.catalogLoading ? "loading your Spotify library…" : searchPending ? "searching…" : searchError ?? "no matching tracks"}</div>
      {/each}
      {#if snapshot.view === "search" && searchCursors[searchKind] && !searchPending}
        <button class="load-more" on:click={() => void searchSpotify(query.trim(), searchKind, true)}>more results</button>
      {/if}
      {/if}
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
          on:input={(event) => previewSeek(Number(event.currentTarget.value))}
          on:change={(event) => commitSeek(Number(event.currentTarget.value))}
        />
        <span>{current ? duration(current.durationMs) : "0:00"}</span>
      </div>
      <div class="now-playing">
        <div class="transport">
          <button class:active={snapshot.playback.shuffle} aria-label="Toggle shuffle" aria-pressed={snapshot.playback.shuffle} on:click={() => void dispatch({ type: "toggle_shuffle" })}>⤨</button>
          <button aria-label="Previous track" on:click={() => void dispatch({ type: "previous" })}>◀</button>
          <button class="play" aria-label={snapshot.playback.playing ? "Pause" : "Play"} aria-pressed={snapshot.playback.playing} on:click={() => void dispatch({ type: "toggle_playback" })}>{snapshot.playback.playing ? "Ⅱ" : "▶"}</button>
          <button aria-label="Next track" on:click={() => void dispatch({ type: "next" })}>▶</button>
          <button class:active={snapshot.playback.repeat !== "off"} aria-label="Cycle repeat" aria-pressed={snapshot.playback.repeat !== "off"} on:click={() => void dispatch({ type: "cycle_repeat" })}>↻</button>
        </div>
        <div class="track-summary">
          <strong>{current?.title ?? "Nothing playing"}</strong>
          <span>{current?.artists.join(", ") ?? "Connect Spotify to begin"}</span>
        </div>
        <label class="volume">
          <span>vol</span>
          <input type="range" min="0" max="1" step="0.01" value={snapshot.playback.volume} aria-label="Volume" on:input={(event) => previewVolume(Number(event.currentTarget.value))} on:change={(event) => commitVolume(Number(event.currentTarget.value))} />
          <span>{percent(snapshot.playback.volume)}</span>
        </label>
      </div>
    </footer>
  </section>

  {#if settingsOpen}
    <aside class="settings" aria-label="Visual preferences">
      <div class="settings-title">space artwork <button aria-label="Close preferences" on:click={() => settingsOpen = false}>×</button></div>
      <fieldset class="artwork-picker">
        <legend>space image</legend>
        <div class="artwork-grid" aria-label="Static space artwork">
          {#each spaceArtworks as artwork}
            <button class:active={selectedArtworkId === artwork.id} aria-label={`Use ${artwork.name}`} title={`${artwork.name} · ${artwork.credit}`} style={`background-image: url("${artwork.src}")`} on:click={() => selectArtwork(artwork.id)}><span>{artwork.name}</span></button>
          {/each}
          <label class:active={selectedArtworkId === "custom"} class="artwork-upload">
            <input type="file" accept="image/jpeg,image/png,image/webp,image/avif" on:change={uploadArtwork} />
            <span>choose image<br /><em>from explorer</em></span>
          </label>
        </div>
        <p class="artwork-credit">{selectedArtworkId === "custom" ? "Your local image · session only" : spaceArtworks.find((artwork) => artwork.id === selectedArtworkId)?.credit}</p>
      </fieldset>
      <button class="focus-mode-action" on:click={enterVisualMode}>open visual mode <span>↗</span></button>
      <p>Artwork stays still. The spectrum follows playback.</p>
      <button class="diagnostics" on:click={exportDiagnostics}>export diagnostics</button>
    </aside>
  {/if}

  {#if preferences.foregroundHidden}
    <div class="visual-mode-return" role="region" aria-label="Visual mode controls">
      <span>static visual</span>
      <button on:click={exitVisualMode}>return to player</button>
    </div>
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

  {#if trackMenu}
    <div class="context-menu" role="menu" aria-label={`Actions for ${trackMenu.track.title}`} style={`left: ${trackMenu.x}px; top: ${trackMenu.y}px`}>
      <header><span>{trackMenu.track.title}</span><button aria-label="Close menu" on:click={() => trackMenu = null}>×</button></header>
      <button role="menuitem" on:click={() => { activate(trackMenu!.track); trackMenu = null; }}>play</button>
      <button role="menuitem" on:click={() => { void queueNext(trackMenu!.track); trackMenu = null; }}>play next</button>
      <button role="menuitem" on:click={() => { void dispatch({ type: "enqueue", trackId: trackMenu!.track.id, track: trackMenu!.track }); statusMessage = `Queued ${trackMenu!.track.title}`; trackMenu = null; }}>queue</button>
      <div class="context-rule"></div>
      <button role="menuitem" on:click={() => searchFromMenu(`artist:${trackMenu!.track.artists[0]}`)}>artist</button>
      <button role="menuitem" on:click={() => searchFromMenu(`album:${trackMenu!.track.album}`)}>show album</button>
      <button role="menuitem" on:click={() => { void copySpotifyLink("track", trackMenu!.track.id); trackMenu = null; }}>share</button>
      <button role="menuitem" on:click={() => searchFromMenu(`${trackMenu!.track.artists[0]} ${trackMenu!.track.title}`)}>similar tracks</button>
      <button role="menuitem" on:click={() => { void dispatch({ type: "set_saved", trackId: trackMenu!.track.id, track: trackMenu!.track, saved: !trackMenu!.track.saved }); trackMenu = null; }}>{trackMenu.track.saved ? "remove from library" : "save"}</button>
      <div class="context-rule"></div>
      <button role="menuitem" class:open={trackMenu.playlistPicker} on:click={() => trackMenu = { ...trackMenu!, playlistPicker: !trackMenu!.playlistPicker }}>add to playlist <span>›</span></button>
      {#if trackMenu.playlistPicker}
        <div class="playlist-picker" aria-label="Choose playlist">
          {#each snapshot.playlists.slice(0, 8) as playlist (playlist.id)}
            <button role="menuitem" on:click={() => { void dispatch({ type: "add_to_playlist", trackId: trackMenu!.track.id, playlistId: playlist.id }); trackMenu = null; }}>{playlist.name}</button>
          {:else}
            <span>no playlists loaded</span>
          {/each}
        </div>
      {/if}
    </div>
  {:else if playlistMenu}
    <div class="context-menu" role="menu" aria-label={`Actions for ${playlistMenu.playlist.name}`} style={`left: ${playlistMenu.x}px; top: ${playlistMenu.y}px`}>
      <header><span>{playlistMenu.playlist.name}</span><button aria-label="Close menu" on:click={() => playlistMenu = null}>×</button></header>
      <button role="menuitem" on:click={() => { activatePlaylist(playlistMenu!.playlist); playlistMenu = null; }}>play</button>
      <button role="menuitem" on:click={() => { void openPlaylist(playlistMenu!.playlist); playlistMenu = null; }}>open playlist</button>
      <div class="context-rule"></div>
      <button role="menuitem" on:click={() => { void copySpotifyLink("playlist", playlistMenu!.playlist.id); playlistMenu = null; }}>share playlist</button>
    </div>
  {/if}

</main>
