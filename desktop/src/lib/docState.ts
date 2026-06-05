/// Per-file UI state persisted across launches — currently just the last
/// query, keyed by absolute path. Mirrors the macOS app's
/// `DocumentStateStore` (LRU-bounded at 64 entries). Read once when a file
/// opens, written when its query changes; no reactivity needed.

const STORAGE_KEY = "BigJSON.documentStates.v1";
const MAX_ENTRIES = 64;

interface Persisted {
  states: Record<string, { query: string }>;
  lru: string[];
}

function load(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed.states === "object" && Array.isArray(parsed.lru)) {
        return parsed as Persisted;
      }
    }
  } catch {
    // fall through to empty
  }
  return { states: {}, lru: [] };
}

const data = load();

function persist() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
  } catch {
    // Quota / private mode: in-memory state still works this session.
  }
}

export const docState = {
  queryFor(path: string): string {
    return data.states[path]?.query ?? "";
  },
  save(path: string, query: string) {
    data.states[path] = { query };
    data.lru = [path, ...data.lru.filter((p) => p !== path)];
    if (data.lru.length > MAX_ENTRIES) {
      for (const stale of data.lru.slice(MAX_ENTRIES)) delete data.states[stale];
      data.lru = data.lru.slice(0, MAX_ENTRIES);
    }
    persist();
  },
};
