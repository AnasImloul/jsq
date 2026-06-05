<script lang="ts">
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { onMount } from "svelte";
  import EmptyState from "./lib/EmptyState.svelte";
  import LoadingView from "./lib/LoadingView.svelte";
  import Navigator from "./lib/Navigator.svelte";
  import QueryBar from "./lib/QueryBar.svelte";
  import QueryResults from "./lib/QueryResults.svelte";
  import StatusBar from "./lib/StatusBar.svelte";
  import TabStrip from "./lib/TabStrip.svelte";
  import {
    closeFile,
    exportQuery,
    formatQuery,
    openFile,
    parseProgress,
    pickExportPath,
    pickFile,
    runQuery,
    textSearch,
    type ExportFormat,
  } from "./lib/api";
  import { docState } from "./lib/docState";
  import { queryStore } from "./lib/queryStore.svelte";
  import { recentFilesStore } from "./lib/recentFilesStore.svelte";
  import { themeStore } from "./lib/themeStore.svelte";
  import type { OpenResult, QueryRun } from "./lib/types";

  const QUERY_LIMIT = 5000;
  const DEBOUNCE_MS = 150;
  const PROGRESS_MS = 100;

  // One open document per tab. `id` is a frontend-only uid that is stable
  // for the tab's whole life (including while it loads, before the backend
  // `docId` exists). `docId` is the backend handle, set once parsing
  // finishes. All per-document UI state lives here so switching tabs is
  // just an `activeId` swap — no reparse, no refetch of computed results.
  interface Tab {
    id: number;
    docId: number | null;
    doc: OpenResult | null;
    fileName: string;
    filePath: string;
    loading: boolean;
    loadParsed: number;
    loadTotal: number;
    loadElapsed: number;
    loadError: string;
    selected: number | null;
    query: string;
    queryResult: QueryRun | null;
    queryError: string;
    queryRunning: boolean;
    queryDuration: number;
    runToken: number;
    debounceTimer?: ReturnType<typeof setTimeout>;
  }

  let tabs = $state<Tab[]>([]);
  let activeId = $state<number | null>(null);
  let focusToken = $state(0);
  let uid = 0;

  const active = $derived(tabs.find((t) => t.id === activeId) ?? null);
  const isSearch = $derived(!!active && active.query.trimStart().startsWith("/"));

  // Mirror the chosen theme onto <html data-theme>, which the CSS variable
  // blocks key off. Until this runs the OS preference applies via @media.
  $effect(() => {
    document.documentElement.dataset.theme = themeStore.theme;
  });

  async function open(path: string) {
    // Focus an already-open file rather than parsing it a second time.
    const existing = tabs.find((t) => t.filePath === path);
    if (existing) {
      activeId = existing.id;
      return;
    }
    // Create and focus the tab immediately so the user sees a loading
    // progress bar in place instead of a blocking full-screen wait.
    const id = uid++;
    const tab: Tab = {
      id,
      docId: null,
      doc: null,
      fileName: path.split(/[\\/]/).pop() ?? path,
      filePath: path,
      loading: true,
      loadParsed: 0,
      loadTotal: 0,
      loadElapsed: 0,
      loadError: "",
      selected: null,
      query: "",
      queryResult: null,
      queryError: "",
      queryRunning: false,
      queryDuration: 0,
      runToken: 0,
    };
    tabs = [...tabs, tab];
    activeId = id;

    const started = performance.now();
    const poll = setInterval(async () => {
      const [parsed, total] = await parseProgress();
      const live = tabs.find((t) => t.id === id);
      if (!live) return;
      live.loadParsed = parsed;
      live.loadTotal = total;
      live.loadElapsed = (performance.now() - started) / 1000;
    }, PROGRESS_MS);

    try {
      const result = await openFile(path);
      const live = tabs.find((t) => t.id === id);
      if (!live) {
        // Tab was closed mid-load; drop the now-orphaned backend doc.
        void closeFile(result.docId);
        return;
      }
      live.docId = result.docId;
      live.doc = result;
      live.selected = result.rootId;
      live.loading = false;
      recentFilesStore.record(path);
      // Restore the last query run against this file and re-run it.
      const savedQuery = docState.queryFor(path);
      if (savedQuery) {
        live.query = savedQuery;
        void runCurrent(live);
      }
    } catch (e) {
      const live = tabs.find((t) => t.id === id);
      if (live) {
        live.loadError = String(e);
        live.loading = false;
      }
    } finally {
      clearInterval(poll);
    }
  }

  async function chooseAndOpen() {
    const path = await pickFile();
    if (path) await open(path);
  }

  async function closeTab(id: number) {
    const idx = tabs.findIndex((t) => t.id === id);
    if (idx < 0) return;
    const docId = tabs[idx].docId;
    clearTimeout(tabs[idx].debounceTimer);
    tabs = tabs.filter((t) => t.id !== id);
    if (activeId === id) {
      const next = tabs[idx] ?? tabs[idx - 1] ?? null;
      activeId = next ? next.id : null;
    }
    if (docId !== null) {
      try {
        await closeFile(docId);
      } catch {
        // doc already gone; nothing to do
      }
    }
  }

  function clearQuery(tab: Tab) {
    clearTimeout(tab.debounceTimer);
    tab.runToken++;
    tab.queryResult = null;
    tab.queryError = "";
    tab.queryRunning = false;
  }

  function onQueryInput(value: string) {
    const tab = active;
    if (!tab) return;
    tab.query = value;
    docState.save(tab.filePath, value);
    clearTimeout(tab.debounceTimer);
    if (value.trim() === "") {
      clearQuery(tab);
      return;
    }
    tab.debounceTimer = setTimeout(() => void runCurrent(tab), DEBOUNCE_MS);
  }

  function submitQuery() {
    const tab = active;
    if (!tab) return;
    clearTimeout(tab.debounceTimer);
    void runCurrent(tab);
  }

  async function runCurrent(tab: Tab) {
    const text = tab.query.trim();
    if (text === "") {
      clearQuery(tab);
      return;
    }
    if (tab.docId === null) return;
    const search = text.startsWith("/");
    const token = ++tab.runToken;
    tab.queryRunning = true;
    tab.queryError = "";
    const started = performance.now();
    try {
      const result = search
        ? await textSearch(tab.docId, text.slice(1).trim(), QUERY_LIMIT)
        : await runQuery(tab.docId, text, QUERY_LIMIT);
      if (token !== tab.runToken) return;
      tab.queryResult = result;
      tab.queryDuration = Math.round(performance.now() - started);
      queryStore.recordRecent(text);
    } catch (e) {
      if (token !== tab.runToken) return;
      tab.queryError = String(e);
      tab.queryResult = null;
    } finally {
      if (token === tab.runToken) tab.queryRunning = false;
    }
  }

  async function onFormat() {
    const tab = active;
    if (!tab) return;
    if (tab.query.trimStart().startsWith("/") || tab.query.trim() === "") return;
    try {
      tab.query = await formatQuery(tab.query);
      docState.save(tab.filePath, tab.query);
      tab.queryError = "";
      submitQuery();
    } catch (e) {
      tab.queryError = String(e);
    }
  }

  function closeResults() {
    const tab = active;
    if (!tab) return;
    tab.query = "";
    docState.save(tab.filePath, "");
    clearQuery(tab);
  }

  // ⌘G / ⇧⌘G: cycle the selection through the selectable result rows.
  function stepResults(direction: number) {
    const tab = active;
    if (!tab || !tab.queryResult) return;
    const ids = tab.queryResult.rows
      .map((r) => r.nodeId)
      .filter((id): id is number => id !== null);
    if (ids.length === 0) return;
    const cur = tab.selected !== null ? ids.indexOf(tab.selected) : -1;
    const next =
      cur >= 0
        ? (cur + direction + ids.length) % ids.length
        : direction >= 0
          ? 0
          : ids.length - 1;
    tab.selected = ids[next];
  }

  function onWindowKey(e: KeyboardEvent) {
    if (e.key === "t" && e.metaKey) {
      e.preventDefault();
      void chooseAndOpen();
      return;
    }
    if (!active) return;
    if (e.key === "w" && e.metaKey) {
      e.preventDefault();
      void closeTab(active.id);
    } else if (e.key === "f" && e.metaKey) {
      e.preventDefault();
      focusToken++;
    } else if (e.key === "g" && e.metaKey) {
      e.preventDefault();
      stepResults(e.shiftKey ? -1 : 1);
    }
  }

  async function onExport(format: ExportFormat) {
    const tab = active;
    if (!tab || tab.docId === null) return;
    const text = tab.query.trim();
    if (text === "") return;
    try {
      const path = await pickExportPath(format);
      if (!path) return;
      await exportQuery(tab.docId, text, QUERY_LIMIT, format, path);
    } catch (e) {
      tab.queryError = String(e);
    }
  }

  onMount(() => {
    const unlistenPromise = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "drop" && event.payload.paths.length > 0) {
        void open(event.payload.paths[0]);
      }
    });
    window.addEventListener("keydown", onWindowKey);
    return () => {
      void unlistenPromise.then((un) => un());
      window.removeEventListener("keydown", onWindowKey);
    };
  });
</script>

<main class:loaded={tabs.length > 0}>
  {#if tabs.length > 0}
    <TabStrip
      tabs={tabs.map((t) => ({ id: t.id, fileName: t.fileName, loading: t.loading }))}
      {activeId}
      onSelect={(id) => (activeId = id)}
      onClose={closeTab}
      onNew={chooseAndOpen}
    />
  {/if}

  {#if active}
    {#key active.id}
      {#if active.loadError}
        <div class="load-error">
          <div class="err-card">
            <div class="err-title">Couldn't open {active.fileName}</div>
            <pre class="err-msg">{active.loadError}</pre>
            <button class="err-dismiss" onclick={() => closeTab(active.id)}>Close tab</button>
          </div>
        </div>
      {:else if active.loading || !active.doc}
        <LoadingView
          fileName={active.fileName}
          parsed={active.loadParsed}
          total={active.loadTotal}
          elapsed={active.loadElapsed}
        />
      {:else}
        <QueryBar
          docId={active.docId ?? 0}
          value={active.query}
          {isSearch}
          running={active.queryRunning}
          error={active.queryError}
          {focusToken}
          onInput={onQueryInput}
          onFormat={onFormat}
          onSubmit={submitQuery}
        />
        <div class="content" class:split={!!active.queryResult}>
          <div class="pane navigator-pane">
            <Navigator
              docId={active.docId ?? 0}
              nodeId={active.selected ?? active.doc.rootId}
              filePath={active.filePath}
              onSelect={(id) => (active.selected = id)}
            />
          </div>
          {#if active.queryResult}
            <div class="pane results-pane">
              <QueryResults
                result={active.queryResult}
                duration={active.queryDuration}
                selected={active.selected}
                canExport={!isSearch}
                limitCap={QUERY_LIMIT}
                doc={active.doc}
                onExport={onExport}
                onSelect={(id) => (active.selected = id)}
                onClose={closeResults}
              />
            </div>
          {/if}
        </div>
        <StatusBar doc={active.doc} selected={active.selected} />
      {/if}
    {/key}
  {:else}
    <EmptyState onOpen={chooseAndOpen} onOpenPath={open} />
  {/if}
</main>

<style>
  :global(:root) {
    --fg-primary: #1d1d1f;
    --fg-secondary: #6e6e73;
    --fg-tertiary: #aeaeb2;
    --accent: #007aff;
    --divider: rgba(0, 0, 0, 0.1);
    --row-hover: rgba(0, 0, 0, 0.05);
    --row-selected: rgba(0, 122, 255, 0.12);
    --value-bg: rgba(0, 0, 0, 0.04);
    --bg: #ffffff;
    --topbar-bg: #f5f5f7;
    --tabstrip-bg: #e4e4e8;
  }
  /* Dark values, shared by the OS-preference default (before/without an
     explicit choice) and the forced-dark override. */
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme])) {
      --fg-primary: #f5f5f7;
      --fg-secondary: #98989d;
      --fg-tertiary: #636366;
      --accent: #0a84ff;
      --divider: rgba(255, 255, 255, 0.12);
      --row-hover: rgba(255, 255, 255, 0.08);
      --row-selected: rgba(10, 132, 255, 0.26);
      --value-bg: rgba(255, 255, 255, 0.06);
      --bg: #1e1e1e;
      --topbar-bg: #2a2a2c;
      --tabstrip-bg: #161617;
    }
  }
  :global(:root[data-theme="dark"]) {
    --fg-primary: #f5f5f7;
    --fg-secondary: #98989d;
    --fg-tertiary: #636366;
    --accent: #0a84ff;
    --divider: rgba(255, 255, 255, 0.12);
    --row-hover: rgba(255, 255, 255, 0.08);
    --row-selected: rgba(10, 132, 255, 0.26);
    --value-bg: rgba(255, 255, 255, 0.06);
    --bg: #1e1e1e;
    --topbar-bg: #2a2a2c;
    --tabstrip-bg: #161617;
  }
  :global(body) {
    margin: 0;
    font-family: -apple-system, system-ui, sans-serif;
    color: var(--fg-primary);
    background: var(--bg);
  }
  main {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .pane {
    min-width: 0;
    min-height: 0;
    height: 100%;
  }
  .navigator-pane {
    flex: 1;
  }
  .content.split .navigator-pane {
    flex: 0 0 42%;
    border-right: 1px solid var(--divider);
  }
  .results-pane {
    flex: 1;
  }
  .load-error {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
  }
  .err-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    width: 420px;
    max-width: 80%;
    text-align: center;
  }
  .err-title {
    font-size: 15px;
    font-weight: 600;
  }
  .err-msg {
    margin: 0;
    width: 100%;
    box-sizing: border-box;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--value-bg);
    border: 1px solid var(--divider);
    color: var(--fg-secondary);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    text-align: left;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .err-dismiss {
    padding: 6px 16px;
    border-radius: 7px;
    border: 1px solid var(--divider);
    background: transparent;
    color: var(--fg-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .err-dismiss:hover {
    background: var(--row-hover);
  }
</style>
