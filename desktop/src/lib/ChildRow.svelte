<script lang="ts">
  import TypeGlyph from "./TypeGlyph.svelte";
  import { kindMeta, type ChildDto } from "./types";

  let {
    child,
    selected,
    onSelect,
  }: {
    child: ChildDto;
    selected: boolean;
    onSelect: (id: number) => void;
  } = $props();

  const selectable = $derived(child.id !== null);
  const meta = $derived(kindMeta(child.kind));

  const label = $derived(
    child.key !== null ? child.key : child.index !== null ? `[${child.index}]` : "$",
  );

  const valueText = $derived.by(() => {
    if (child.isContainer) {
      const n = child.childCount;
      return child.kind === 5
        ? `{ ${n} ${n === 1 ? "key" : "keys"} }`
        : `[ ${n} ${n === 1 ? "item" : "items"} ]`;
    }
    return child.preview + (child.truncated ? "…" : "");
  });

  function activate() {
    if (child.id !== null) onSelect(child.id);
  }
</script>

{#snippet content()}
  <TypeGlyph kind={child.kind} size="small" />
  <span class="key">{label}</span>
  <span class="val" style:color={child.isContainer ? "var(--fg-secondary)" : meta.color}>
    {valueText}
  </span>
{/snippet}

{#if selectable}
  <button class="row selectable" class:selected onclick={activate}>
    {@render content()}
  </button>
{:else}
  <div class="row">
    {@render content()}
  </div>
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
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  .row.selectable {
    cursor: pointer;
  }
  .row.selectable:hover {
    background: var(--row-hover);
  }
  .row.selected {
    background: var(--row-selected);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .key {
    color: var(--fg-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 45%;
  }
  .val {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: right;
  }
</style>
