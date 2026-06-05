<script lang="ts">
  import { onMount } from "svelte";
  import { homeDir } from "@tauri-apps/api/path";
  import { engineVersion } from "./api";
  import { recentFilesStore } from "./recentFilesStore.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";

  let { onOpen, onOpenPath }: { onOpen: () => void; onOpenPath: (path: string) => void } =
    $props();

  let version = $state("");
  let home = $state("");

  const recents = $derived(recentFilesStore.entries.slice(0, 6));

  onMount(() => {
    void engineVersion()
      .then((v) => (version = v))
      .catch(() => {});
    // Best-effort: used only to abbreviate the home directory to "~".
    void homeDir()
      .then((h) => (home = h.replace(/[\\/]+$/, "")))
      .catch(() => {});
  });

  function dirDisplay(path: string): string {
    const dir = path.slice(0, Math.max(0, path.lastIndexOf("/")));
    if (home && dir.startsWith(home)) return "~" + dir.slice(home.length);
    return dir;
  }
</script>

<div class="empty">
  <div class="corner-tools">
    <ThemeToggle />
  </div>
  <div class="logo">{`{ }`}</div>
  <h1>BigJSON</h1>
  <p>Open a JSON file to explore it.</p>
  <button class="open-btn" onclick={onOpen}>Open File…</button>
  <p class="hint">or drag a file onto this window</p>

  {#if recents.length > 0}
    <div class="recents">
      <div class="recents-head">
        <span class="recents-label">Recent</span>
        <button class="recents-clear" onclick={() => recentFilesStore.clear()}>Clear</button>
      </div>
      <div class="recents-list">
        {#each recents as entry (entry.path)}
          <button class="recent-row" title={entry.path} onclick={() => onOpenPath(entry.path)}>
            <span class="recent-name">{recentFilesStore.fileName(entry.path)}</span>
            <span class="recent-dir">{dirDisplay(entry.path)}</span>
          </button>
        {/each}
      </div>
    </div>
  {/if}

  {#if version}
    <span class="version">engine {version}</span>
  {/if}
</div>

<style>
  .empty {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: 10px;
    color: var(--fg-secondary);
  }
  .corner-tools {
    position: absolute;
    top: 8px;
    right: 8px;
  }
  .logo {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 56px;
    font-weight: 700;
    color: var(--accent);
  }
  h1 {
    margin: 0;
    font-size: 22px;
    color: var(--fg-primary);
  }
  p {
    margin: 0;
    font-size: 14px;
  }
  .open-btn {
    margin-top: 10px;
    padding: 8px 18px;
    font-size: 14px;
    border-radius: 7px;
    border: none;
    background: var(--accent);
    color: #fff;
    cursor: pointer;
  }
  .open-btn:hover {
    filter: brightness(1.08);
  }
  .hint {
    font-size: 12px;
    color: var(--fg-tertiary);
  }
  .recents {
    width: 420px;
    max-width: 80%;
    margin-top: 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .recents-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .recents-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-secondary);
  }
  .recents-clear {
    border: none;
    background: transparent;
    color: var(--fg-tertiary);
    font-size: 11px;
    cursor: pointer;
  }
  .recents-clear:hover {
    color: var(--fg-primary);
  }
  .recents-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 6px;
    border-radius: 8px;
    background: var(--value-bg);
  }
  .recent-row {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 6px 8px;
    border: none;
    border-radius: 6px;
    background: transparent;
    text-align: left;
    cursor: pointer;
    overflow: hidden;
  }
  .recent-row:hover {
    background: var(--row-hover);
  }
  .recent-name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--fg-primary);
  }
  .recent-dir {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--fg-secondary);
  }
  .version {
    position: absolute;
    bottom: 12px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    color: var(--fg-tertiary);
  }
</style>
