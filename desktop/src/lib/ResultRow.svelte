<script lang="ts">
  import Self from "./ResultRow.svelte";
  import TypeGlyph from "./TypeGlyph.svelte";
  import { formatCount } from "./format";
  import {
    containerCount,
    containerSummary,
    eagerChildren,
    entryForChild,
    type RowEntry,
  } from "./resultsTree";
  import type { TreeState } from "./treeState.svelte";
  import { kindMeta } from "./types";

  let {
    entry,
    depth,
    tree,
    selected,
    onSelect,
  }: {
    entry: RowEntry;
    depth: number;
    tree: TreeState;
    selected: number | null;
    onSelect: (id: number) => void;
  } = $props();

  const isContainer = $derived(entry.payload.kind === "container");
  const expanded = $derived(tree.isExpanded(entry.path));
  const isSelected = $derived(entry.nodeId !== null && entry.nodeId === selected);

  const summary = $derived.by(() => {
    if (entry.payload.kind !== "container") return "";
    const n = containerCount(entry.payload.source);
    return containerSummary(entry.payload.container, n);
  });

  const eager = $derived.by(() => {
    if (entry.payload.kind !== "container" || entry.payload.source.kind !== "eager") return [];
    return eagerChildren(entry.path, entry.payload.container, entry.payload.source.entries);
  });

  // Lazy-load document-backed children once the row is expanded.
  $effect(() => {
    if (
      entry.payload.kind === "container" &&
      entry.payload.source.kind === "lazy" &&
      expanded
    ) {
      void tree.ensure(entry.path, entry.payload.source.nodeId, entry.payload.source.total);
    }
  });

  const lazy = $derived(
    entry.payload.kind === "container" && entry.payload.source.kind === "lazy"
      ? tree.lazyState(entry.path)
      : undefined,
  );

  function toggle(e: MouseEvent) {
    e.stopPropagation();
    tree.toggle(entry.path);
  }

  function activate() {
    if (entry.nodeId !== null) onSelect(entry.nodeId);
  }

  function onRowKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      activate();
    } else if (e.key === "ArrowRight" && isContainer && !expanded) {
      tree.toggle(entry.path);
    } else if (e.key === "ArrowLeft" && isContainer && expanded) {
      tree.toggle(entry.path);
    }
  }

  function loadMore() {
    if (entry.payload.kind === "container" && entry.payload.source.kind === "lazy") {
      void tree.loadMore(entry.path, entry.payload.source.nodeId);
    }
  }
</script>

<div
  class="row"
  class:selected={isSelected}
  style:padding-left="{8 + depth * 14}px"
  role="button"
  tabindex="0"
  onclick={activate}
  onkeydown={onRowKey}
>
  {#if isContainer}
    <button class="chevron" class:open={expanded} title="Expand" onclick={toggle}>›</button>
  {:else}
    <span class="chevron-spacer"></span>
  {/if}
  <TypeGlyph kind={entry.type} size="small" />
  {#if entry.mode.kind === "named"}
    <span class="name">{entry.mode.name}</span>
    <span class="secondary">
      {#if entry.payload.kind === "scalar"}{entry.payload.text}{:else}{summary}{/if}
    </span>
  {:else if entry.payload.kind === "scalar"}
    <span class="value" style:color={kindMeta(entry.type).color}>{entry.payload.text}</span>
  {:else}
    <span class="value" style:color={kindMeta(entry.type).color}>{summary}</span>
  {/if}
</div>

{#if expanded && entry.payload.kind === "container"}
  {#if entry.payload.source.kind === "eager"}
    {#each eager as child (child.path)}
      <Self entry={child} depth={depth + 1} {tree} {selected} {onSelect} />
    {/each}
  {:else if lazy}
    {#each lazy.rows as child, i (entry.path + "/" + i)}
      <Self
        entry={entryForChild(child, i, entry.path)}
        depth={depth + 1}
        {tree}
        {selected}
        {onSelect}
      />
    {/each}
    {#if lazy.loaded < lazy.total}
      <button class="load-more" style:padding-left="{8 + (depth + 1) * 14}px" onclick={loadMore}>
        {lazy.loading
          ? "Loading…"
          : `Load ${formatCount(Math.min(500, lazy.total - lazy.loaded))} of ${formatCount(lazy.total - lazy.loaded)} more…`}
      </button>
    {/if}
  {/if}
{/if}

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    box-sizing: border-box;
    padding: 5px 8px;
    border: none;
    border-radius: 4px;
    background: none;
    text-align: left;
    color: inherit;
    cursor: pointer;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .row.selected {
    background: var(--row-selected);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 12px;
    height: 12px;
    flex-shrink: 0;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--fg-secondary);
    font-size: 13px;
    cursor: pointer;
    transition: transform 0.1s ease;
  }
  .chevron.open {
    transform: rotate(90deg);
  }
  .chevron-spacer {
    width: 12px;
    flex-shrink: 0;
  }
  .name {
    color: var(--fg-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 45%;
  }
  .secondary {
    flex: 1;
    color: var(--fg-secondary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: right;
  }
  .value {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .load-more {
    display: block;
    width: 100%;
    box-sizing: border-box;
    padding: 5px 8px;
    border: none;
    background: none;
    text-align: left;
    color: var(--fg-secondary);
    cursor: pointer;
    font-size: 12px;
  }
  .load-more:hover {
    background: var(--row-hover);
  }
</style>
