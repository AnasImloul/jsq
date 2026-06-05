<script lang="ts">
  export interface QueryListItem {
    id: string;
    query: string;
    /// Display label shown above the query body (saved queries only).
    label: string | null;
  }

  let {
    title,
    entries,
    emptyMessage,
    onSelect,
    onDelete,
    onClearAll,
  }: {
    title: string;
    entries: QueryListItem[];
    emptyMessage: string;
    onSelect: (item: QueryListItem) => void;
    onDelete: (item: QueryListItem) => void;
    onClearAll: () => void;
  } = $props();
</script>

<div class="popover">
  <header class="phdr">
    <span class="ptitle">{title}</span>
    <span class="pcount">{entries.length}</span>
    {#if entries.length > 0}
      <button class="clear" onclick={onClearAll}>Clear All</button>
    {/if}
  </header>
  {#if entries.length === 0}
    <div class="pempty">{emptyMessage}</div>
  {:else}
    <ul class="plist">
      {#each entries as item (item.id)}
        <li class="pitem">
          <button class="pselect" type="button" onclick={() => onSelect(item)}>
            {#if item.label}
              <span class="plabel">{item.label}</span>
            {/if}
            <span class="pquery" class:secondary={item.label !== null}>{item.query}</span>
          </button>
          <button
            class="pdelete"
            type="button"
            title={item.label !== null ? "Unbookmark" : "Remove from history"}
            onclick={() => onDelete(item)}>×</button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .popover {
    display: flex;
    flex-direction: column;
    width: 420px;
    max-height: 420px;
    background: var(--bg);
    border: 1px solid var(--divider);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
    overflow: hidden;
  }
  .phdr {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--topbar-bg);
    border-bottom: 1px solid var(--divider);
  }
  .ptitle {
    font-weight: 600;
    font-size: 13px;
  }
  .pcount {
    color: var(--fg-tertiary);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .clear {
    margin-left: auto;
    border: none;
    background: transparent;
    color: var(--accent);
    font-size: 12px;
    cursor: pointer;
    padding: 0;
  }
  .pempty {
    padding: 24px 16px;
    text-align: center;
    color: var(--fg-tertiary);
    font-size: 12px;
  }
  .plist {
    margin: 0;
    padding: 0;
    list-style: none;
    overflow-y: auto;
  }
  .pitem {
    display: flex;
    align-items: flex-start;
    border-bottom: 1px solid var(--divider);
  }
  .pitem:hover {
    background: var(--row-hover);
  }
  .pitem:hover .pdelete {
    opacity: 1;
  }
  .pselect {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 12px;
    border: none;
    background: none;
    text-align: left;
    cursor: pointer;
    color: inherit;
  }
  .plabel {
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pquery {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    color: var(--fg-primary);
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }
  .pquery.secondary {
    color: var(--fg-secondary);
  }
  .pdelete {
    flex-shrink: 0;
    align-self: center;
    margin-right: 8px;
    width: 18px;
    height: 18px;
    border: none;
    border-radius: 50%;
    background: transparent;
    color: var(--fg-tertiary);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.1s ease;
  }
  .pdelete:hover {
    color: var(--fg-primary);
    background: var(--row-hover);
  }
</style>
