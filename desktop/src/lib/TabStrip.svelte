<script lang="ts">
  import ThemeToggle from "./ThemeToggle.svelte";

  let {
    tabs,
    activeId,
    onSelect,
    onClose,
    onNew,
  }: {
    tabs: { id: number; fileName: string; loading: boolean }[];
    activeId: number | null;
    onSelect: (id: number) => void;
    onClose: (id: number) => void;
    onNew: () => void;
  } = $props();
</script>

<div class="tabstrip">
  <div class="tabs" role="tablist">
    {#each tabs as tab (tab.id)}
      <div
        class="tab"
        class:active={tab.id === activeId}
        role="tab"
        tabindex="0"
        aria-selected={tab.id === activeId}
        title={tab.fileName}
        onclick={() => onSelect(tab.id)}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onSelect(tab.id);
          }
        }}
      >
        {#if tab.loading}
          <span class="tab-spinner" aria-label="loading"></span>
        {/if}
        <span class="tab-name">{tab.fileName}</span>
        <button
          class="tab-close"
          title="Close tab"
          aria-label="Close tab"
          onclick={(e) => {
            e.stopPropagation();
            onClose(tab.id);
          }}
        >
          ×
        </button>
      </div>
    {/each}
    <button class="tab-new" title="Open file" aria-label="Open file" onclick={onNew}>+</button>
  </div>
  <div class="tab-tools">
    <ThemeToggle />
  </div>
</div>

<style>
  .tabstrip {
    display: flex;
    align-items: stretch;
    height: 34px;
    background: var(--tabstrip-bg);
    border-bottom: 1px solid var(--divider);
  }
  .tabs {
    display: flex;
    align-items: stretch;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar {
    display: none;
  }
  .tab-tools {
    display: flex;
    align-items: center;
    flex-shrink: 0;
    padding: 0 5px;
    border-left: 1px solid var(--divider);
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 220px;
    padding: 0 8px 0 12px;
    border-right: 1px solid var(--divider);
    font-size: 12px;
    color: var(--fg-secondary);
    cursor: pointer;
    user-select: none;
    flex-shrink: 0;
  }
  .tab:hover {
    background: var(--row-hover);
  }
  .tab.active {
    background: var(--bg);
    color: var(--fg-primary);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .tab-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tab-spinner {
    width: 10px;
    height: 10px;
    flex-shrink: 0;
    border: 1.5px solid var(--divider);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: tab-spin 0.7s linear infinite;
  }
  @keyframes tab-spin {
    to {
      transform: rotate(360deg);
    }
  }
  .tab-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--fg-tertiary);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
  }
  .tab:hover .tab-close,
  .tab.active .tab-close {
    opacity: 1;
  }
  .tab-close:hover {
    background: var(--divider);
    color: var(--fg-primary);
  }
  .tab-new {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    flex-shrink: 0;
    border: none;
    background: transparent;
    color: var(--fg-secondary);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }
  .tab-new:hover {
    background: var(--row-hover);
    color: var(--fg-primary);
  }
</style>
