/// Copies `text` to the system clipboard. Uses the webview's async
/// Clipboard API (available in the Tauri webview without a plugin).
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Clipboard denied / unavailable: nothing actionable to surface.
  }
}

/// POSIX single-quote shell-escaping, matching the macOS app's
/// `Inspector.shellQuote` so the "Copy as jq command" output is
/// paste-safe.
export function shellQuote(s: string): string {
  return "'" + s.replaceAll("'", "'\\''") + "'";
}
