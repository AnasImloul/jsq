/// App-wide appearance preference, persisted to localStorage. Mirrors the
/// macOS app's three-way `AppTheme` (system / light / dark): "system"
/// follows the OS `prefers-color-scheme` live, while "light"/"dark" force a
/// choice that sticks. App.svelte mirrors the *resolved* `theme` onto
/// `<html data-theme>`, which the CSS variable blocks key off.

const THEME_KEY = "theme";

export type ThemePref = "system" | "light" | "dark";
export type Theme = "light" | "dark";

function loadPref(): ThemePref {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "system" || saved === "light" || saved === "dark") return saved;
  } catch {
    // fall through to system default
  }
  return "system";
}

function systemTheme(): Theme {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

const ORDER: ThemePref[] = ["system", "light", "dark"];

function createThemeStore() {
  let pref = $state<ThemePref>(loadPref());
  let system = $state<Theme>(systemTheme());

  // Keep tracking the OS preference so "system" mode updates live.
  window.matchMedia?.("(prefers-color-scheme: dark)").addEventListener?.("change", (e) => {
    system = e.matches ? "dark" : "light";
  });

  function set(next: ThemePref) {
    pref = next;
    try {
      localStorage.setItem(THEME_KEY, pref);
    } catch {
      // Quota / private mode: choice still applies this session.
    }
  }

  return {
    get pref(): ThemePref {
      return pref;
    },
    /// The concrete theme to render — "system" resolves against the OS.
    get theme(): Theme {
      return pref === "system" ? system : pref;
    },
    set,
    /// Advance system → light → dark → system.
    cycle() {
      set(ORDER[(ORDER.indexOf(pref) + 1) % ORDER.length]);
    },
  };
}

export const themeStore = createThemeStore();
