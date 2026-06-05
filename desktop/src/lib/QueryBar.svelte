<script lang="ts">
  import { onMount, tick } from "svelte";
  import { completionContext, grammarManifest, querySchema, tokenizeQuery } from "./api";
  import QueryListPopover, { type QueryListItem } from "./QueryListPopover.svelte";
  import { queryStore } from "./queryStore.svelte";
  import type { CompletionContext, FieldSchema, Token } from "./types";

  let {
    docId,
    value,
    isSearch,
    running,
    error,
    focusToken,
    onInput,
    onFormat,
    onSubmit,
  }: {
    docId: number;
    value: string;
    isSearch: boolean;
    running: boolean;
    error: string;
    focusToken: number;
    onInput: (value: string) => void;
    onFormat: () => void;
    onSubmit: () => void;
  } = $props();

  const SAMPLE_LIMIT = 5000;
  const KIND_ARRAY_BIT = 1 << 4;
  const KIND_OBJECT_BIT = 1 << 5;
  const ARRAY_ACCESSORS = ["[]", "[0]", "[-1]"];

  type Sug = { text: string; kind: "key" | "arrayAccessor" | "builtin" };

  let textarea = $state<HTMLTextAreaElement | null>(null);
  let mirror = $state<HTMLDivElement | null>(null);
  let focused = $state(false);

  let tokens = $state<Token[]>([]);
  let valueKeywords = $state<string[]>([]);
  let infixKeywords = $state<string[]>([]);

  let suggestions = $state<Sug[]>([]);
  let selectedIndex = $state(0);
  let dismissed = $state(true);
  let caret = $state({ top: 0, left: 0 });
  let lastCtx = $state<CompletionContext | null>(null);

  const schemaCache = new Map<string, FieldSchema>();
  const inFlight = new Set<string>();
  let refreshToken = 0;
  let ignoreNextChange = false;

  const popupVisible = $derived(
    focused && !isSearch && !dismissed && suggestions.length > 0,
  );

  // Saved / recent query lists.
  let showSaved = $state(false);
  let showRecent = $state(false);
  let actionsHost = $state<HTMLDivElement | null>(null);

  const isCurrentSaved = $derived(queryStore.isSaved(value));

  const savedItems = $derived<QueryListItem[]>(
    queryStore.saved.map((e) => ({ id: e.id, query: e.query, label: e.name })),
  );
  const recentItems = $derived<QueryListItem[]>(
    queryStore.recent.map((q) => ({ id: `recent:${q}`, query: q, label: null })),
  );

  function closePopovers() {
    showSaved = false;
    showRecent = false;
  }

  function toggleBookmark() {
    if (value.trim() === "") return;
    queryStore.toggleSaved(value);
  }

  function chooseQuery(item: QueryListItem) {
    closePopovers();
    onInput(item.query);
  }

  // Dismiss the saved/recent popovers on any outside click.
  $effect(() => {
    if (!showSaved && !showRecent) return;
    function onDocMouseDown(e: MouseEvent) {
      if (actionsHost && !actionsHost.contains(e.target as Node)) closePopovers();
    }
    document.addEventListener("mousedown", onDocMouseDown);
    return () => document.removeEventListener("mousedown", onDocMouseDown);
  });

  onMount(async () => {
    try {
      const m = await grammarManifest();
      const vs = m.keywords
        .filter((k) => k.role === "valueStart" || k.role === "both")
        .map((k) => k.text);
      const ix = m.keywords
        .filter((k) => k.role === "afterExpression" || k.role === "both")
        .map((k) => k.text);
      valueKeywords = [...new Set(vs)].sort();
      infixKeywords = [...new Set(ix)].sort();
    } catch {
      // Empty manifest: highlighting still works, autocomplete stays quiet.
    }
  });

  // Re-tokenize for the highlight overlay whenever the text changes.
  $effect(() => {
    const text = value;
    if (text === "" || isSearch) {
      tokens = [];
      return;
    }
    let cancelled = false;
    tokenizeQuery(text)
      .then((t) => {
        if (!cancelled) tokens = t;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  // Auto-grow the textarea to fit its content so multi-line (formatted)
  // queries are fully visible and the overlay stays aligned.
  $effect(() => {
    value;
    if (!textarea) return;
    textarea.style.height = "auto";
    textarea.style.height = `${textarea.scrollHeight}px`;
  });

  // ⌘F (handled in App) bumps focusToken to pull focus into the field
  // and select all, so the user can immediately overtype.
  $effect(() => {
    if (focusToken <= 0 || !textarea) return;
    textarea.focus();
    textarea.select();
  });

  const segments = $derived.by(() => {
    const text = value;
    const segs: { text: string; category: string }[] = [];
    let pos = 0;
    const sorted = [...tokens].sort((a, b) => a.offset - b.offset);
    for (const t of sorted) {
      if (t.offset < pos) continue;
      if (t.offset > pos) segs.push({ text: text.slice(pos, t.offset), category: "plain" });
      segs.push({ text: text.slice(t.offset, t.offset + t.length), category: t.category });
      pos = t.offset + t.length;
    }
    if (pos < text.length) segs.push({ text: text.slice(pos), category: "plain" });
    return segs;
  });

  function cursorPos(): number {
    return textarea?.selectionStart ?? value.length;
  }

  function handleInput(e: Event) {
    const ta = e.currentTarget as HTMLTextAreaElement;
    if (ignoreNextChange) ignoreNextChange = false;
    else dismissed = false;
    onInput(ta.value);
    void refresh(ta.value, ta.selectionStart ?? ta.value.length);
  }

  // Caret moved without typing (click / arrow): keep popup hidden but
  // still refresh context so the next keystroke has the right scope.
  function handleNav() {
    dismissed = true;
    void refresh(value, cursorPos());
  }

  async function refresh(text: string, cur: number) {
    if (isSearch) {
      suggestions = [];
      lastCtx = null;
      return;
    }
    const token = ++refreshToken;
    const ctx = await completionContext(text, cur);
    if (token !== refreshToken) return;
    lastCtx = ctx;
    if (!ctx) {
      suggestions = [];
      selectedIndex = 0;
      return;
    }

    let candidates: Sug[] = [];
    if (ctx.mode === "fieldAccess") {
      const cq = ctx.contextQuery ?? ".";
      const cached = schemaCache.get(cq);
      if (!cached) {
        void loadSchema(cq);
        suggestions = [];
        selectedIndex = 0;
        return;
      }
      candidates = fieldCandidates(cached);
    } else if (ctx.mode === "valueStart") {
      candidates = valueKeywords.map((t) => ({ text: t, kind: "builtin" }));
    } else {
      candidates = infixKeywords.map((t) => ({ text: t, kind: "builtin" }));
    }

    const partial = ctx.partial.toLowerCase();
    const filtered =
      partial === ""
        ? candidates
        : candidates.filter((c) => c.text.toLowerCase().startsWith(partial));
    suggestions = filtered.slice(0, 20);
    if (selectedIndex >= suggestions.length) selectedIndex = 0;
    void updateCaret();
  }

  async function loadSchema(cq: string) {
    if (inFlight.has(cq)) return;
    inFlight.add(cq);
    try {
      schemaCache.set(cq, await querySchema(docId, cq, SAMPLE_LIMIT));
    } catch {
      // leave uncached; a later keystroke retries
    } finally {
      inFlight.delete(cq);
    }
    if (focused) void refresh(value, cursorPos());
  }

  function fieldCandidates(s: FieldSchema): Sug[] {
    const out: Sug[] = [];
    const hasObj = (s.kinds & KIND_OBJECT_BIT) !== 0;
    const hasArr = (s.kinds & KIND_ARRAY_BIT) !== 0;
    if (hasObj || (!hasArr && s.keys.length > 0)) {
      out.push(...s.keys.map((k): Sug => ({ text: k, kind: "key" })));
    }
    if (hasArr) {
      out.push(...ARRAY_ACCESSORS.map((a): Sug => ({ text: a, kind: "arrayAccessor" })));
    }
    return out;
  }

  async function applySuggestion(index: number) {
    if (!textarea || !lastCtx) return;
    const s = suggestions[index];
    if (!s) return;
    const text = value;
    const cur = Math.max(0, Math.min(textarea.selectionStart ?? text.length, text.length));
    const head = text.slice(0, cur);
    const tail = text.slice(cur);
    let headWithoutPartial = head.slice(0, Math.max(0, head.length - lastCtx.partialUtf16Length));
    const trimmedTail = tail.replace(/^[A-Za-z0-9_]+/, "");

    let replacement: string;
    if (s.kind === "key") {
      if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(s.text)) {
        replacement = s.text;
      } else {
        if (headWithoutPartial.endsWith(".")) headWithoutPartial = headWithoutPartial.slice(0, -1);
        replacement = `[${JSON.stringify(s.text)}]`;
      }
    } else if (s.kind === "arrayAccessor") {
      if (headWithoutPartial.endsWith(".")) headWithoutPartial = headWithoutPartial.slice(0, -1);
      replacement = s.text;
    } else {
      replacement = s.text;
    }

    const newText = headWithoutPartial + replacement + trimmedTail;
    const newCursor = headWithoutPartial.length + replacement.length;
    ignoreNextChange = true;
    dismissed = true;
    onInput(newText);
    await tick();
    if (textarea) {
      textarea.selectionStart = textarea.selectionEnd = newCursor;
      textarea.focus();
    }
  }

  async function updateCaret() {
    await tick();
    if (!textarea || !mirror) return;
    const cur = textarea.selectionStart ?? 0;
    mirror.textContent = value.slice(0, cur);
    const marker = document.createElement("span");
    marker.textContent = "​";
    mirror.appendChild(marker);
    caret = {
      top: marker.offsetTop - textarea.scrollTop,
      left: marker.offsetLeft - textarea.scrollLeft,
    };
    mirror.removeChild(marker);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "l" && (e.metaKey || e.altKey)) {
      e.preventDefault();
      onFormat();
      return;
    }
    if (e.key === "s" && e.metaKey) {
      e.preventDefault();
      toggleBookmark();
      return;
    }
    if (popupVisible) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        selectedIndex = (selectedIndex + 1) % suggestions.length;
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        selectedIndex = (selectedIndex - 1 + suggestions.length) % suggestions.length;
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        void applySuggestion(selectedIndex);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        dismissed = true;
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      onSubmit();
    }
    // Shift+Enter falls through to insert a newline.
  }
</script>

<div class="bar">
  <div class="field" class:search={isSearch}>
    <span class="icon">{isSearch ? "⌕" : "›"}</span>
    <div class="editor">
      <div class="highlight" aria-hidden="true">{#each segments as seg}<span class={`cat-${seg.category}`}>{seg.text}</span>{/each}</div>
      <div class="mirror" aria-hidden="true" bind:this={mirror}></div>
      <textarea
        bind:this={textarea}
        class="input"
        class:plain={isSearch}
        rows="1"
        spellcheck="false"
        autocapitalize="off"
        placeholder={`/ for text search · from .users[] as u where u.active aggregate { n: count() }`}
        {value}
        oninput={handleInput}
        onkeydown={handleKeydown}
        onkeyup={handleNav}
        onclick={handleNav}
        onfocus={() => (focused = true)}
        onblur={() => {
          focused = false;
          dismissed = true;
        }}
      ></textarea>
      {#if popupVisible}
        <ul class="popup" style:top={`${caret.top + 22}px`} style:left={`${caret.left}px`}>
          {#each suggestions as sug, i (sug.kind + sug.text)}
            <li>
              <button
                type="button"
                class="sug"
                class:active={i === selectedIndex}
                onmousedown={(e) => {
                  e.preventDefault();
                  void applySuggestion(i);
                }}
              >
                <span class="sug-text">{sug.text}</span>
                <span class="sug-kind">{sug.kind === "key" ? "key" : sug.kind === "arrayAccessor" ? "[]" : "kw"}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    {#if running}<span class="spinner" aria-label="running"></span>{/if}
    <div class="actions" bind:this={actionsHost}>
      <button
        class="fmt"
        title="Format query (⌥L)"
        disabled={isSearch || value.trim() === ""}
        onclick={onFormat}>{`{ }`}</button>
      <button
        class="iconbtn"
        class:on={isCurrentSaved}
        title={isCurrentSaved ? "Remove from saved (⌘S)" : "Save query (⌘S)"}
        disabled={value.trim() === ""}
        onclick={toggleBookmark}>{isCurrentSaved ? "★" : "☆"}</button>
      <div class="pop-wrap">
        <button
          class="iconbtn"
          class:on={showSaved}
          title="Saved queries"
          onclick={() => {
            showRecent = false;
            showSaved = !showSaved;
          }}>▤</button>
        {#if showSaved}
          <div class="pop-anchor">
            <QueryListPopover
              title="Saved"
              entries={savedItems}
              emptyMessage="No saved queries yet — press ⌘S to save the current one."
              onSelect={chooseQuery}
              onDelete={(item) => queryStore.removeSavedById(item.id)}
              onClearAll={() => queryStore.clearSaved()}
            />
          </div>
        {/if}
      </div>
      <div class="pop-wrap">
        <button
          class="iconbtn"
          class:on={showRecent}
          title="Recent queries"
          onclick={() => {
            showSaved = false;
            showRecent = !showRecent;
          }}>↺</button>
        {#if showRecent}
          <div class="pop-anchor">
            <QueryListPopover
              title="Recent"
              entries={recentItems}
              emptyMessage="No recent queries yet."
              onSelect={chooseQuery}
              onDelete={(item) => queryStore.removeRecent(item.query)}
              onClearAll={() => queryStore.clearRecent()}
            />
          </div>
        {/if}
      </div>
    </div>
  </div>
  {#if error}
    <div class="err">{error}</div>
  {/if}
</div>

<style>
  .bar {
    display: flex;
    flex-direction: column;
    border-bottom: 1px solid var(--divider);
    background: var(--topbar-bg);
    --tok-keyword: #af52de;
    --tok-string: #d70015;
    --tok-number: #007aff;
    --tok-splat: #b35900;
  }
  @media (prefers-color-scheme: dark) {
    .bar {
      --tok-keyword: #d18cf0;
      --tok-string: #ff6961;
      --tok-number: #4aa3ff;
      --tok-splat: #ff9f0a;
    }
  }
  .field {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 6px 12px;
  }
  .icon {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 14px;
    color: var(--fg-secondary);
    width: 14px;
    text-align: center;
    padding-top: 2px;
  }
  .field.search .icon {
    color: var(--accent);
  }
  .editor {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  /* The overlay, mirror, and textarea must share identical text metrics
     so colored spans line up exactly under the (transparent) input. */
  .highlight,
  .mirror,
  .input {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    line-height: 1.5;
    padding: 2px 0;
    margin: 0;
    border: 0;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-wrap: anywhere;
    box-sizing: border-box;
    width: 100%;
  }
  .highlight {
    position: absolute;
    inset: 0;
    pointer-events: none;
    color: var(--fg-primary);
  }
  .mirror {
    position: absolute;
    inset: 0;
    visibility: hidden;
    pointer-events: none;
  }
  .input {
    position: relative;
    display: block;
    resize: none;
    outline: none;
    background: transparent;
    color: transparent;
    caret-color: var(--fg-primary);
    overflow: hidden;
  }
  .input.plain {
    color: var(--fg-primary);
  }
  .input::placeholder {
    color: var(--fg-tertiary);
  }
  .cat-keyword,
  .cat-reducer,
  .cat-literal,
  .cat-splat {
    font-weight: 600;
  }
  .cat-keyword,
  .cat-reducer,
  .cat-literal {
    color: var(--tok-keyword);
  }
  .cat-string {
    color: var(--tok-string);
  }
  .cat-number {
    color: var(--tok-number);
  }
  .cat-splat {
    color: var(--tok-splat);
  }
  .cat-comment {
    color: var(--fg-secondary);
    font-style: italic;
  }
  .cat-operator {
    color: var(--fg-secondary);
  }
  .cat-punctuation {
    color: var(--fg-tertiary);
  }
  .cat-plain,
  .cat-identifier,
  .cat-error {
    color: var(--fg-primary);
  }
  .popup {
    position: absolute;
    z-index: 20;
    margin: 0;
    padding: 4px;
    list-style: none;
    min-width: 160px;
    max-width: 320px;
    max-height: 240px;
    overflow-y: auto;
    background: var(--bg);
    border: 1px solid var(--divider);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
  }
  .sug {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 4px 8px;
    border: none;
    border-radius: 5px;
    background: none;
    text-align: left;
    cursor: pointer;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    color: var(--fg-primary);
  }
  .sug.active {
    background: var(--accent);
    color: #fff;
  }
  .sug-text {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sug-kind {
    font-size: 10px;
    opacity: 0.6;
  }
  .actions {
    display: flex;
    align-items: flex-start;
    gap: 4px;
    flex-shrink: 0;
  }
  .fmt {
    flex-shrink: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    padding: 2px 8px;
    border-radius: 5px;
    border: 1px solid var(--divider);
    background: transparent;
    color: var(--fg-secondary);
    cursor: pointer;
  }
  .fmt:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .fmt:not(:disabled):hover {
    color: var(--fg-primary);
    background: var(--row-hover);
  }
  .iconbtn {
    flex-shrink: 0;
    font-size: 13px;
    line-height: 1;
    padding: 3px 7px;
    border-radius: 5px;
    border: 1px solid var(--divider);
    background: transparent;
    color: var(--fg-secondary);
    cursor: pointer;
  }
  .iconbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .iconbtn:not(:disabled):hover {
    color: var(--fg-primary);
    background: var(--row-hover);
  }
  .iconbtn.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .pop-wrap {
    position: relative;
  }
  .pop-anchor {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 40;
  }
  .spinner {
    width: 12px;
    height: 12px;
    margin-top: 3px;
    border: 2px solid var(--divider);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
    flex-shrink: 0;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .err {
    padding: 6px 12px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    color: #ff453a;
    border-top: 1px solid var(--divider);
    white-space: pre-wrap;
  }
</style>
