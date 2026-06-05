<script lang="ts">
  import TypeGlyph from "./TypeGlyph.svelte";
  import Breadcrumb from "./Breadcrumb.svelte";
  import ChildRow from "./ChildRow.svelte";
  import { fetchAncestors, fetchChildren, fetchNodeDetail } from "./api";
  import { copyText, shellQuote } from "./clipboard";
  import { formatBytes, formatCount } from "./format";
  import { kindMeta, type Ancestor, type ChildDto, type NodeDetail } from "./types";

  const PAGE = 500;

  let {
    docId,
    nodeId,
    filePath,
    onSelect,
  }: {
    docId: number;
    nodeId: number;
    filePath: string;
    onSelect: (id: number) => void;
  } = $props();

  function copyPath() {
    if (detail) void copyText(detail.path);
  }

  function copyJq() {
    if (detail) void copyText(`jq ${shellQuote(detail.path)} ${shellQuote(filePath)}`);
  }

  function copyValue() {
    if (detail?.value != null) void copyText(detail.value);
  }

  let detail = $state<NodeDetail | null>(null);
  let chain = $state<Ancestor[]>([]);
  let rows = $state<ChildDto[]>([]);
  let loadedCount = $state(0);
  let loading = $state(false);
  let sentinel = $state<HTMLDivElement | null>(null);

  const meta = $derived(detail ? kindMeta(detail.kind) : null);

  const summary = $derived.by(() => {
    if (!detail) return "";
    const n = detail.childCount;
    switch (detail.kind) {
      case 5:
        return `${n} ${n === 1 ? "key" : "keys"}`;
      case 4:
        return `${n} ${n === 1 ? "item" : "items"}`;
      case 3: {
        const inner = detail.byteLength >= 2 ? detail.byteLength - 2 : 0;
        return formatBytes(inner);
      }
      default:
        return detail.value ?? "";
    }
  });

  // Reload everything whenever the selected node changes.
  $effect(() => {
    const id = nodeId;
    void load(id);
  });

  async function load(id: number) {
    rows = [];
    loadedCount = 0;
    const [d, a] = await Promise.all([fetchNodeDetail(docId, id), fetchAncestors(docId, id)]);
    if (id !== nodeId) return;
    detail = d;
    chain = a;
    if (d.isContainer && d.childCount > 0) {
      await loadMore(id);
    }
  }

  async function loadMore(id: number) {
    if (loading) return;
    if (detail && loadedCount >= detail.childCount) return;
    loading = true;
    try {
      const page = await fetchChildren(docId, id, loadedCount, PAGE);
      if (id !== nodeId) return;
      rows = [...rows, ...page];
      loadedCount += page.length;
    } finally {
      loading = false;
    }
  }

  // Infinite scroll: fire loadMore when the sentinel scrolls into view.
  $effect(() => {
    const el = sentinel;
    if (!el) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) void loadMore(nodeId);
    });
    io.observe(el);
    return () => io.disconnect();
  });

  function select(id: number) {
    onSelect(id);
  }

  const hasMore = $derived(!!detail && loadedCount < detail.childCount);
</script>

<div class="navigator">
  {#if detail && meta}
    <header class="hdr">
      <TypeGlyph kind={detail.kind} />
      <div class="hdr-text">
        <div class="hdr-title">{meta.label}</div>
        <div class="hdr-summary">{summary}</div>
      </div>
    </header>

    <hr />

    <section class="path">
      <div class="section-label">Path</div>
      <div class="path-row">
        <Breadcrumb {chain} onSelect={select} />
        <div class="copy-btns">
          <button class="copy-btn" title="Copy path" onclick={copyPath}>⧉</button>
          <button class="copy-btn" title="Copy as jq command" onclick={copyJq}>›_</button>
        </div>
      </div>
    </section>

    <hr />

    {#if detail.isContainer}
      <section class="children">
        <div class="section-label">Children — {formatCount(detail.childCount)}</div>
        {#if detail.childCount === 0}
          <div class="muted">Empty</div>
        {:else}
          <div class="rows">
            {#each rows as child, i (i)}
              <ChildRow
                {child}
                selected={child.id !== null && child.id === nodeId}
                onSelect={select}
              />
            {/each}
            {#if hasMore}
              <div class="sentinel" bind:this={sentinel}>
                {loading ? "Loading…" : ""}
              </div>
            {/if}
          </div>
        {/if}
      </section>
    {:else}
      <section class="value">
        <div class="value-head">
          <div class="section-label">Value</div>
          {#if detail.value !== null}
            <button class="copy-btn" title="Copy value" onclick={copyValue}>⧉</button>
          {/if}
        </div>
        {#if detail.value !== null}
          <pre class="value-box" style:color={meta.color}>{detail.value}</pre>
        {:else}
          <div class="muted">—</div>
        {/if}
      </section>
    {/if}

    <hr />

    <section class="metadata">
      <div class="section-label">Metadata</div>
      <div class="meta-row"><span class="meta-key">Type</span><span class="meta-val">{meta.label}</span></div>
      <div class="meta-row"><span class="meta-key">Byte offset</span><span class="meta-val">{formatCount(detail.byteOffset)}</span></div>
      <div class="meta-row"><span class="meta-key">Byte length</span><span class="meta-val">{formatBytes(detail.byteLength)}</span></div>
      {#if detail.isContainer}
        <div class="meta-row"><span class="meta-key">Children</span><span class="meta-val">{formatCount(detail.childCount)}</span></div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .navigator {
    overflow-y: auto;
    height: 100%;
  }
  hr {
    border: none;
    border-top: 1px solid var(--divider);
    margin: 0 16px;
  }
  .hdr {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 16px;
  }
  .hdr-title {
    font-size: 17px;
    font-weight: 600;
  }
  .hdr-summary {
    font-size: 13px;
    color: var(--fg-secondary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  section {
    padding: 12px 16px;
  }
  .section-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--fg-secondary);
    margin-bottom: 6px;
  }
  .path-row {
    display: flex;
    align-items: flex-start;
    gap: 8px;
  }
  .path-row :global(> :first-child) {
    flex: 1;
    min-width: 0;
  }
  .copy-btns {
    display: inline-flex;
    gap: 2px;
    flex-shrink: 0;
  }
  .value-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 6px;
  }
  .value-head .section-label {
    margin-bottom: 0;
  }
  .copy-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 22px;
    height: 22px;
    padding: 0 5px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--fg-secondary);
    font-size: 12px;
    cursor: pointer;
  }
  .copy-btn:hover {
    color: var(--fg-primary);
    background: var(--row-hover);
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .muted {
    color: var(--fg-secondary);
    font-size: 13px;
  }
  .sentinel {
    padding: 6px 8px;
    font-size: 11px;
    color: var(--fg-secondary);
  }
  .value-box {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
    padding: 10px;
    border-radius: 6px;
    background: var(--value-bg);
    border: 0.5px solid var(--divider);
    user-select: text;
  }
  .meta-row {
    display: flex;
    align-items: baseline;
    margin-bottom: 4px;
  }
  .meta-key {
    width: 96px;
    font-size: 11px;
    color: var(--fg-secondary);
    flex-shrink: 0;
  }
  .meta-val {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    user-select: text;
  }
</style>
