<script lang="ts">
  import { formatCount } from "./format";
  import { kindMeta, type TableCell, type TableSnapshot } from "./types";

  let {
    table,
    selected,
    onSelect,
  }: {
    table: TableSnapshot;
    selected: number | null;
    onSelect: (id: number) => void;
  } = $props();

  function cellSummary(cell: TableCell): string {
    if (cell.isContainer) {
      const n = cell.count ?? 0;
      return cell.kind === 5
        ? `{ ${formatCount(n)} ${n === 1 ? "key" : "keys"} }`
        : `[ ${formatCount(n)} ${n === 1 ? "item" : "items"} ]`;
    }
    return cell.text ?? "";
  }
</script>

<div class="table-wrap">
  <table>
    <thead>
      <tr>
        <th class="name-col">Name</th>
        {#each table.columns as col (col)}
          <th>{col}</th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each table.rows as row, i (i)}
        {@const selectable = row.nodeId !== null}
        <tr
          class:selectable
          class:selected={selectable && row.nodeId === selected}
          onclick={() => selectable && onSelect(row.nodeId!)}
        >
          <td class="name-col">{row.label}</td>
          {#each table.columns as col (col)}
            {@const cell = row.cells[col]}
            <td>
              {#if cell}
                <span
                  class="cell"
                  style:color={cell.isContainer ? "var(--fg-secondary)" : kindMeta(cell.kind).color}
                >
                  {cellSummary(cell)}
                </span>
              {/if}
            </td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .table-wrap {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
  }
  thead th {
    position: sticky;
    top: 0;
    z-index: 1;
    text-align: left;
    font-weight: 600;
    color: var(--fg-secondary);
    background: var(--topbar-bg);
    padding: 6px 10px;
    border-bottom: 1px solid var(--divider);
    white-space: nowrap;
  }
  tbody td {
    padding: 5px 10px;
    border-bottom: 1px solid var(--divider);
    white-space: nowrap;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  tbody tr:nth-child(even) td {
    background: var(--row-hover);
  }
  tbody tr:nth-child(even) .name-col {
    background: linear-gradient(var(--row-hover), var(--row-hover)), var(--bg);
  }
  tr.selectable {
    cursor: pointer;
  }
  tr.selectable:hover td {
    background: var(--row-hover);
  }
  tbody tr.selected td {
    background: var(--row-selected);
  }
  .name-col {
    font-weight: 600;
    color: var(--fg-primary);
    position: sticky;
    left: 0;
    background: var(--bg);
  }
  thead .name-col {
    background: var(--topbar-bg);
  }
  tbody tr.selected .name-col {
    background: linear-gradient(var(--row-selected), var(--row-selected)), var(--bg);
    box-shadow: inset 2px 0 0 var(--accent);
  }
</style>
