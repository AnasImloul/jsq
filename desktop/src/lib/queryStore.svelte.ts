/// App-wide persisted query lists: bookmarked ("saved") queries and an
/// MRU history of successful runs ("recent"). Mirrors the macOS app's
/// `SavedQueriesStore` (UserDefaults "savedQueries.v1") and
/// `QueryModel` recent list (UserDefaults "recentQueries", cap 20).
/// Backed by localStorage; a single shared instance is exported below.

const SAVED_KEY = "savedQueries.v1";
const RECENT_KEY = "recentQueries";
const RECENT_CAP = 20;

export interface SavedEntry {
  id: string;
  query: string;
  /// Optional human label. UI falls back to the query text when null.
  name: string | null;
}

function trim(s: string): string {
  return s.trim();
}

function loadSaved(): SavedEntry[] {
  try {
    const raw = localStorage.getItem(SAVED_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is SavedEntry => e && typeof e.query === "string" && typeof e.id === "string",
    );
  } catch {
    return [];
  }
}

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((q): q is string => typeof q === "string") : [];
  } catch {
    return [];
  }
}

function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function createQueryStore() {
  let saved = $state<SavedEntry[]>(loadSaved());
  let recent = $state<string[]>(loadRecent());

  function persistSaved() {
    try {
      localStorage.setItem(SAVED_KEY, JSON.stringify(saved));
    } catch {
      // Quota / private mode: in-memory state still works this session.
    }
  }

  function persistRecent() {
    try {
      localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
    } catch {
      // ignore
    }
  }

  return {
    get saved(): SavedEntry[] {
      return saved;
    },
    get recent(): string[] {
      return recent;
    },

    isSaved(query: string): boolean {
      const t = trim(query);
      return saved.some((e) => e.query === t);
    },

    /// Bookmark `query`. De-dupes on text, bumping an existing entry to
    /// the top rather than accumulating duplicates.
    addSaved(query: string, name: string | null = null) {
      const t = trim(query);
      if (t === "") return;
      saved = [{ id: newId(), query: t, name }, ...saved.filter((e) => e.query !== t)];
      persistSaved();
    },

    removeSavedById(id: string) {
      saved = saved.filter((e) => e.id !== id);
      persistSaved();
    },

    removeSavedByQuery(query: string) {
      const t = trim(query);
      saved = saved.filter((e) => e.query !== t);
      persistSaved();
    },

    /// Toggle the current query's bookmarked state.
    toggleSaved(query: string) {
      if (this.isSaved(query)) this.removeSavedByQuery(query);
      else this.addSaved(query);
    },

    clearSaved() {
      saved = [];
      persistSaved();
    },

    /// Record a successfully-run query at the head of the MRU list.
    recordRecent(query: string) {
      const t = trim(query);
      if (t === "") return;
      recent = [t, ...recent.filter((q) => q !== t)].slice(0, RECENT_CAP);
      persistRecent();
    },

    removeRecent(query: string) {
      recent = recent.filter((q) => q !== query);
      persistRecent();
    },

    clearRecent() {
      recent = [];
      persistRecent();
    },
  };
}

export type QueryStore = ReturnType<typeof createQueryStore>;

/// Shared singleton — every document view reads the same lists, matching
/// the macOS app's app-wide stores.
export const queryStore = createQueryStore();
