<script lang="ts">
  import { kindMeta } from "./types";

  let { kind, size = "large" }: { kind: number; size?: "small" | "large" } = $props();

  const meta = $derived(kindMeta(kind));
  const dim = $derived(size === "large" ? 32 : 18);
  const font = $derived(size === "large" ? 11 : 8);
  const corner = $derived(size === "large" ? 6 : 3);
</script>

<span
  class="glyph"
  style:width="{dim}px"
  style:height="{dim}px"
  style:font-size="{font}px"
  style:border-radius="{corner}px"
  style:color={meta.color}
  style:background-color="{meta.color}2e"
>
  {meta.badge}
</span>

<style>
  .glyph {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-weight: 700;
    flex-shrink: 0;
  }
</style>
