<script lang="ts">
  import { formatBytes } from "./format";

  let {
    fileName,
    parsed,
    total,
    elapsed,
  }: {
    fileName: string;
    parsed: number;
    total: number;
    elapsed: number;
  } = $props();

  const fraction = $derived(total > 0 ? Math.min(1, Math.max(0, parsed / total)) : 0);
  const percent = $derived(Math.round(fraction * 100));

  // Rate-anchored ETA; only shown once we have a stable sample.
  const eta = $derived.by(() => {
    if (elapsed < 0.5 || parsed <= 0 || total <= 0 || parsed >= total) return null;
    const rate = parsed / elapsed;
    if (rate <= 0) return null;
    return Math.ceil((total - parsed) / rate);
  });

  const detail = $derived.by(() => {
    if (total <= 0) return "Reading…";
    const sizes = `${formatBytes(parsed)} of ${formatBytes(total)} · ${percent}%`;
    return eta !== null ? `${sizes} · ~${eta} sec left` : sizes;
  });
</script>

<div class="loading">
  <div class="card">
    <div class="name">{fileName}</div>
    <div class="track">
      <div class="fill" style:width={`${percent}%`}></div>
    </div>
    <div class="detail">{detail}</div>
  </div>
</div>

<style>
  .loading {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 360px;
    max-width: 70%;
  }
  .name {
    font-size: 14px;
    font-weight: 600;
    text-align: center;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .track {
    height: 6px;
    border-radius: 3px;
    background: var(--value-bg);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.1s linear;
  }
  .detail {
    font-size: 12px;
    color: var(--fg-secondary);
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
</style>
