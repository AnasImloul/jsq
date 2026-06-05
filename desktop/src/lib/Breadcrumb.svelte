<script lang="ts">
  import type { Ancestor } from "./types";

  let {
    chain,
    onSelect,
  }: { chain: Ancestor[]; onSelect: (id: number) => void } = $props();
</script>

<nav class="crumbs">
  {#each chain as entry, i (entry.id)}
    {#if i > 0}
      <span class="sep">›</span>
    {/if}
    {#if i === chain.length - 1}
      <span class="leaf">{entry.label}</span>
    {:else}
      <button class="seg" onclick={() => onSelect(entry.id)}>{entry.label}</button>
    {/if}
  {/each}
</nav>

<style>
  .crumbs {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  .sep {
    color: var(--fg-tertiary);
    font-size: 11px;
  }
  .leaf {
    color: var(--fg-primary);
  }
  .seg {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--accent);
    font: inherit;
  }
  .seg:hover {
    text-decoration: underline;
  }
</style>
