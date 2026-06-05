<script lang="ts">
  import { untrack } from "svelte";
  import ResultRow from "./ResultRow.svelte";
  import ResultsTable from "./ResultsTable.svelte";
  import QueryStatsPopover from "./QueryStatsPopover.svelte";
  import { buildTopRows } from "./resultsTree";
  import { createTreeState } from "./treeState.svelte";
  import { formatBytes, formatCount } from "./format";
  import type { ExportFormat } from "./api";
  import type { OpenResult, QueryRun } from "./types";

  let {
    result,
    duration,
    selected,
    canExport,
    limitCap,
    doc,
    onExport,
    onSelect,
    onClose,
  }: {
    result: QueryRun;
    duration: number;
    selected: number | null;
    canExport: boolean;
    limitCap: number;
    doc: OpenResult | null;
    onExport: (format: ExportFormat) => void;
    onSelect: (id: number) => void;
    onClose: () => void;
  } = $props();

  let showStats = $state(false);

  let showExport = $state(false);
  let exportHost = $state<HTMLDivElement | null>(null);

  function pickExport(format: ExportFormat) {
    showExport = false;
    onExport(format);
  }

  $effect(() => {
    if (!showExport) return;
    function onDocMouseDown(e: MouseEvent) {
      if (exportHost && !exportHost.contains(e.target as Node)) showExport = false;
    }
    document.addEventListener("mousedown", onDocMouseDown);
    return () => document.removeEventListener("mousedown", onDocMouseDown);
  });

  // The tree is keyed per-tab in App, so capturing docId once at mount is
  // correct; untrack documents that the initial value is intentional.
  const tree = createTreeState(untrack(() => doc?.docId ?? 0));

  type Mode = "table" | "list";
  let mode = $state<Mode>("list");
  // Track which result object the current mode/expansion state belongs to,
  // so a new query resets the tree and re-picks the auto-mode exactly once.
  let lastResult: QueryRun | null = null;

  const count = $derived(result.rows.length);
  const topRows = $derived(buildTopRows(result.rows));

  $effect(() => {
    const r = result;
    if (r === lastResult) return;
    lastResult = r;
    tree.reset();
    if (r.table.isTabular) {
      mode = "table";
      // Auto-expand a lone container in list view if the user switches.
    } else {
      if (mode === "table") mode = "list";
      if (topRows.length === 1 && topRows[0].payload.kind === "container") {
        tree.expand(topRows[0].path);
      }
    }
  });

  const canTable = $derived(result.table.columns.length > 0);
</script>

<div class="results">
  <header class="hdr">
    <span class="count">
      {formatCount(count)}{result.hitLimit ? "+" : ""}
      {count === 1 ? "result" : "results"}
    </span>
    <span class="dur">{duration} ms</span>
    <span class="stats">
      {formatCount(result.scannedRows)} scanned · {formatBytes(result.scannedBytes)}
      {#if result.lookupCalls > 0}· {formatCount(result.lookupCalls)} lookups{/if}
    </span>
    <div
      class="stats-wrap"
      onmouseenter={() => (showStats = true)}
      onmouseleave={() => (showStats = false)}
      role="group"
    >
      <button class="icon-btn" title="Query statistics">ⓘ</button>
      {#if showStats}
        <div class="stats-anchor">
          <QueryStatsPopover
            {duration}
            count={result.rows.length}
            hitLimit={result.hitLimit}
            {limitCap}
            scannedRows={result.scannedRows}
            scannedBytes={result.scannedBytes}
            lookupCalls={result.lookupCalls}
            {doc}
          />
        </div>
      {/if}
    </div>
    <span class="spacer"></span>

    {#if count > 0}
      <div class="picker">
        <button class:active={mode === "table"} disabled={!canTable} onclick={() => (mode = "table")} title="Table view">
          ▦
        </button>
        <button class:active={mode === "list"} onclick={() => (mode = "list")} title="List view">
          ☰
        </button>
      </div>
      {#if canExport}
        <div class="export-wrap" bind:this={exportHost}>
          <button class="icon-btn" title="Export query results" onclick={() => (showExport = !showExport)}>⤓</button>
          {#if showExport}
            <ul class="export-menu">
              <li><button onclick={() => pickExport("json")}>JSON Array…</button></li>
              <li><button onclick={() => pickExport("ndjson")}>NDJSON…</button></li>
              <li><button onclick={() => pickExport("csv")}>CSV…</button></li>
            </ul>
          {/if}
        </div>
      {/if}
    {/if}

    <button class="close" title="Close results" onclick={onClose}>×</button>
  </header>

  {#if result.missingIndex}
    <div class="notice">
      No index for <code>{result.missingIndex[0]}</code> on
      <code>{result.missingIndex[1]}</code> — results may be empty.
    </div>
  {/if}

  {#if count === 0}
    <div class="empty">No matches. The filter parsed but produced no rows.</div>
  {:else if mode === "table" && canTable}
    <ResultsTable table={result.table} {selected} {onSelect} />
  {:else}
    <div class="rows">
      {#each topRows as row (row.path)}
        <ResultRow entry={row} depth={0} {tree} {selected} {onSelect} />
      {/each}
    </div>
  {/if}
</div>

<style>
  .results {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .hdr {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--divider);
    font-size: 12px;
  }
  .count {
    font-weight: 600;
  }
  .dur,
  .stats {
    color: var(--fg-secondary);
  }
  .stats {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
  }
  .picker {
    display: inline-flex;
    border: 1px solid var(--divider);
    border-radius: 6px;
    overflow: hidden;
  }
  .picker button {
    border: none;
    background: transparent;
    color: var(--fg-secondary);
    padding: 2px 8px;
    cursor: pointer;
    font-size: 13px;
  }
  .picker button.active {
    background: var(--accent);
    color: #fff;
  }
  .picker button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .icon-btn {
    border: 1px solid var(--divider);
    border-radius: 6px;
    background: transparent;
    color: var(--fg-secondary);
    padding: 2px 6px;
    cursor: pointer;
    font-size: 11px;
    line-height: 1;
  }
  .icon-btn:hover {
    color: var(--fg-primary);
  }
  .stats-wrap {
    position: relative;
    display: inline-flex;
  }
  .stats-anchor {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 40;
  }
  .export-wrap {
    position: relative;
    display: inline-flex;
  }
  .export-menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 40;
    margin: 0;
    padding: 4px;
    list-style: none;
    min-width: 140px;
    background: var(--bg);
    border: 1px solid var(--divider);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
  }
  .export-menu button {
    display: block;
    width: 100%;
    padding: 5px 10px;
    border: none;
    border-radius: 5px;
    background: none;
    text-align: left;
    color: var(--fg-primary);
    font-size: 12px;
    cursor: pointer;
  }
  .export-menu button:hover {
    background: var(--accent);
    color: #fff;
  }
  .close {
    flex-shrink: 0;
    border: none;
    background: transparent;
    color: var(--fg-secondary);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    padding: 0 4px;
  }
  .close:hover {
    color: var(--fg-primary);
  }
  .notice {
    padding: 6px 12px;
    font-size: 12px;
    color: var(--fg-secondary);
    background: var(--value-bg);
    border-bottom: 1px solid var(--divider);
  }
  .notice code {
    color: var(--fg-primary);
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 8px;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .empty {
    padding: 12px;
    color: var(--fg-secondary);
    font-size: 13px;
  }
</style>
