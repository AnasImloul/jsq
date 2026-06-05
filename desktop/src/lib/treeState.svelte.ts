import { fetchChildren } from "./api";
import type { ChildDto } from "./types";

const PAGE = 500;

export interface LazyState {
  rows: ChildDto[];
  loaded: number;
  total: number;
  loading: boolean;
}

/// Shared expansion + lazy-pagination state for the result tree. Held by
/// QueryResults and threaded down through the recursive rows so the whole
/// tree shares one source of truth (and survives row remounts).
export function createTreeState(docId: number) {
  let expanded = $state(new Set<string>());
  let lazy = $state(new Map<string, LazyState>());

  return {
    isExpanded(path: string): boolean {
      return expanded.has(path);
    },
    toggle(path: string) {
      const next = new Set(expanded);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      expanded = next;
    },
    expand(path: string) {
      if (expanded.has(path)) return;
      const next = new Set(expanded);
      next.add(path);
      expanded = next;
    },
    reset() {
      expanded = new Set();
      lazy = new Map();
    },
    lazyState(path: string): LazyState | undefined {
      return lazy.get(path);
    },
    async ensure(path: string, nodeId: number, total: number) {
      if (lazy.has(path)) return;
      const seed = new Map(lazy);
      seed.set(path, { rows: [], loaded: 0, total, loading: true });
      lazy = seed;
      const page = await fetchChildren(docId, nodeId, 0, Math.min(PAGE, total));
      const next = new Map(lazy);
      next.set(path, { rows: page, loaded: page.length, total, loading: false });
      lazy = next;
    },
    async loadMore(path: string, nodeId: number) {
      const cur = lazy.get(path);
      if (!cur || cur.loading || cur.loaded >= cur.total) return;
      const marking = new Map(lazy);
      marking.set(path, { ...cur, loading: true });
      lazy = marking;
      const page = await fetchChildren(docId, nodeId, cur.loaded, Math.min(PAGE, cur.total - cur.loaded));
      const next = new Map(lazy);
      const c = next.get(path)!;
      next.set(path, {
        rows: [...c.rows, ...page],
        loaded: c.loaded + page.length,
        total: c.total,
        loading: false,
      });
      lazy = next;
    },
  };
}

export type TreeState = ReturnType<typeof createTreeState>;
