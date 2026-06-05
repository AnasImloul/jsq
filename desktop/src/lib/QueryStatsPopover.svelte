<script lang="ts">
  import { formatBytes, formatCount } from "./format";
  import type { OpenResult } from "./types";

  let {
    duration,
    count,
    hitLimit,
    limitCap,
    scannedRows,
    scannedBytes,
    lookupCalls,
    doc,
  }: {
    duration: number;
    count: number;
    hitLimit: boolean;
    limitCap: number;
    scannedRows: number;
    scannedBytes: number;
    lookupCalls: number;
    doc: OpenResult | null;
  } = $props();

  function bytesScanned(): string {
    const head = formatBytes(scannedBytes);
    if (!doc || doc.fileSize <= 0) return head;
    const pct = Math.round((scannedBytes / doc.fileSize) * 100);
    return `${head} / ${formatBytes(doc.fileSize)} · ${pct}%`;
  }
</script>

<div class="stats-popover">
  <div class="grp-label">Query</div>
  <dl class="grid">
    <dt>Elapsed</dt>
    <dd>{duration} ms</dd>
    <dt>Scanned</dt>
    <dd>{formatCount(scannedRows)} {scannedRows === 1 ? "row" : "rows"}</dd>
    {#if scannedBytes > 0}
      <dt>Bytes read</dt>
      <dd>{bytesScanned()}</dd>
    {/if}
    {#if lookupCalls > 0}
      <dt>Lookups</dt>
      <dd>{formatCount(lookupCalls)}</dd>
    {/if}
    <dt>Output</dt>
    <dd>
      {#if hitLimit}{formatCount(count)}+ rows · cap reached{:else}{formatCount(count)} {count === 1 ? "row" : "rows"}{/if}
    </dd>
    <dt>Row cap</dt>
    <dd>{formatCount(limitCap)}</dd>
  </dl>
  {#if doc}
    <hr />
    <div class="grp-label">Document</div>
    <dl class="grid">
      <dt>File size</dt>
      <dd>{formatBytes(doc.fileSize)}</dd>
      <dt>Total nodes</dt>
      <dd>{formatCount(doc.totalNodeCount)}</dd>
    </dl>
  {/if}
</div>

<style>
  .stats-popover {
    width: 260px;
    padding: 12px 14px;
    background: var(--bg);
    border: 1px solid var(--divider);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
  }
  .grp-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--fg-secondary);
    margin-bottom: 6px;
  }
  .grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 14px;
    margin: 0;
  }
  dt {
    font-size: 11px;
    color: var(--fg-secondary);
  }
  dd {
    margin: 0;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--fg-primary);
  }
  hr {
    border: none;
    border-top: 1px solid var(--divider);
    margin: 10px 0;
  }
</style>
