/// Persisted list of recently-opened files, newest first. Mirrors the
/// macOS app's `RecentFilesStore` (cap 12). The native app stored
/// security-scoped bookmarks for sandbox re-access; here a plain absolute
/// path is enough to re-open. Backed by localStorage; shared singleton.

const STORAGE_KEY = "BigJSON.recentFiles.v1";
const MAX_ENTRIES = 12;

export interface RecentEntry {
  path: string;
  lastOpened: number;
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function loadEntries(): RecentEntry[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is RecentEntry =>
        e && typeof e.path === "string" && typeof e.lastOpened === "number",
    );
  } catch {
    return [];
  }
}

function createRecentFilesStore() {
  let entries = $state<RecentEntry[]>(loadEntries());

  function persist() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
    } catch {
      // Quota / private mode: in-memory list still works this session.
    }
  }

  return {
    get entries(): RecentEntry[] {
      return entries;
    },
    fileName,
    /// Record a successful open, moving an existing path to the top.
    record(path: string) {
      entries = [
        { path, lastOpened: Date.now() },
        ...entries.filter((e) => e.path !== path),
      ].slice(0, MAX_ENTRIES);
      persist();
    },
    remove(path: string) {
      entries = entries.filter((e) => e.path !== path);
      persist();
    },
    clear() {
      entries = [];
      persist();
    },
  };
}

export const recentFilesStore = createRecentFilesStore();
