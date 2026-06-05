<script lang="ts">
  import { fetchNodeDetail } from "./api";
  import { formatBytes, formatCount } from "./format";
  import { kindMeta, type NodeDetail, type OpenResult } from "./types";

  let {
    doc,
    selected,
  }: {
    doc: OpenResult;
    selected: number | null;
  } = $props();

  let detail = $state<NodeDetail | null>(null);

  // Resolve the selected node's type + path for the right-hand readout.
  $effect(() => {
    const id = selected;
    if (id === null) {
      detail = null;
      return;
    }
    let cancelled = false;
    fetchNodeDetail(doc.docId, id)
      .then((d) => {
        if (!cancelled) detail = d;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

</script>

<footer class="statusbar">
  <span class="chip" title="File size">▤ {formatBytes(doc.fileSize)}</span>
  <span class="chip" title="Total nodes in the document">
    ⬡ {formatCount(doc.totalNodeCount)} nodes
  </span>
  <span class="spacer"></span>
  {#if detail}
    <span class="sel">
      <span class="sel-type">{kindMeta(detail.kind).label}</span>
      <span class="sel-arrow">→</span>
      <span class="sel-path" title={detail.path}>{detail.path}</span>
    </span>
  {/if}
</footer>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: 14px;
    height: 24px;
    padding: 0 12px;
    background: var(--topbar-bg);
    border-top: 1px solid var(--divider);
    font-size: 11px;
    color: var(--fg-secondary);
    flex-shrink: 0;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
  }
  .sel {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .sel-type,
  .sel-arrow {
    color: var(--fg-tertiary);
    flex-shrink: 0;
  }
  .sel-path {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: right;
  }
</style>
