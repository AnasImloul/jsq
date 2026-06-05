<script lang="ts">
  import { themeStore } from "./themeStore.svelte";

  const ICON: Record<string, string> = { system: "◐", light: "☀", dark: "☾" };
  const LABEL: Record<string, string> = { system: "System", light: "Light", dark: "Dark" };

  const icon = $derived(ICON[themeStore.pref]);
  const label = $derived(LABEL[themeStore.pref]);
</script>

<button
  class="theme-toggle"
  title={`Appearance: ${label}`}
  aria-label={`Appearance: ${label}`}
  onclick={() => themeStore.cycle()}
>
  {icon}
</button>

<style>
  .theme-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    flex-shrink: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--fg-secondary);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
  }
  .theme-toggle:hover {
    background: var(--row-hover);
    color: var(--fg-primary);
  }
</style>
