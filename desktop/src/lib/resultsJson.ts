// Order-preserving JSON parser used to lift synthetic result rows
// (aggregate / bucket outputs we never indexed) into an expandable tree.
// JSON.parse loses object key order, which matters for outputs the user
// wrote in a specific order, so we parse by hand. Port of the macOS
// app's `ResultsJSON`.

import { KIND_NULL, KIND_BOOL, KIND_NUMBER, KIND_STRING, KIND_ARRAY, KIND_OBJECT } from "./types";

export type ResultsNode =
  | { node: "scalar"; type: number; text: string }
  | { node: "container"; container: "object" | "array"; entries: [string, ResultsNode][] };

export function parseResultsJSON(s: string): ResultsNode | null {
  const c = { chars: Array.from(s), pos: 0 };
  skipWs(c);
  const n = parseValue(c);
  return n;
}

interface Cursor {
  chars: string[];
  pos: number;
}

function current(c: Cursor): string | null {
  return c.pos < c.chars.length ? c.chars[c.pos] : null;
}

function advance(c: Cursor): string | null {
  if (c.pos >= c.chars.length) return null;
  return c.chars[c.pos++];
}

function skipWs(c: Cursor): void {
  while (c.pos < c.chars.length && /\s/.test(c.chars[c.pos])) c.pos++;
}

function match(c: Cursor, expected: string): boolean {
  if (current(c) === expected) {
    c.pos++;
    return true;
  }
  return false;
}

function matchKeyword(c: Cursor, kw: string): boolean {
  if (c.pos + kw.length > c.chars.length) return false;
  for (let i = 0; i < kw.length; i++) {
    if (c.chars[c.pos + i] !== kw[i]) return false;
  }
  c.pos += kw.length;
  return true;
}

function parseValue(c: Cursor): ResultsNode | null {
  skipWs(c);
  const ch = current(c);
  if (ch === null) return null;
  switch (ch) {
    case "{":
      return parseObject(c);
    case "[":
      return parseArray(c);
    case '"': {
      const s = parseString(c);
      if (s === null) return null;
      return { node: "scalar", type: KIND_STRING, text: '"' + s + '"' };
    }
    case "t":
      return matchKeyword(c, "true") ? { node: "scalar", type: KIND_BOOL, text: "true" } : null;
    case "f":
      return matchKeyword(c, "false") ? { node: "scalar", type: KIND_BOOL, text: "false" } : null;
    case "n":
      return matchKeyword(c, "null") ? { node: "scalar", type: KIND_NULL, text: "null" } : null;
    default:
      if (ch === "-" || (ch >= "0" && ch <= "9")) return parseNumber(c);
      return null;
  }
}

function parseObject(c: Cursor): ResultsNode | null {
  if (!match(c, "{")) return null;
  const entries: [string, ResultsNode][] = [];
  skipWs(c);
  if (match(c, "}")) return { node: "container", container: "object", entries };
  for (;;) {
    skipWs(c);
    const key = parseString(c);
    if (key === null) return null;
    skipWs(c);
    if (!match(c, ":")) return null;
    const value = parseValue(c);
    if (value === null) return null;
    entries.push([key, value]);
    skipWs(c);
    if (match(c, ",")) continue;
    if (match(c, "}")) return { node: "container", container: "object", entries };
    return null;
  }
}

function parseArray(c: Cursor): ResultsNode | null {
  if (!match(c, "[")) return null;
  const entries: [string, ResultsNode][] = [];
  skipWs(c);
  if (match(c, "]")) return { node: "container", container: "array", entries };
  let idx = 0;
  for (;;) {
    const value = parseValue(c);
    if (value === null) return null;
    entries.push([String(idx), value]);
    idx++;
    skipWs(c);
    if (match(c, ",")) continue;
    if (match(c, "]")) return { node: "container", container: "array", entries };
    return null;
  }
}

function parseString(c: Cursor): string | null {
  if (!match(c, '"')) return null;
  let out = "";
  for (;;) {
    const ch = advance(c);
    if (ch === null) return null;
    if (ch === '"') return out;
    if (ch === "\\") {
      const esc = advance(c);
      if (esc === null) return null;
      switch (esc) {
        case '"':
          out += '"';
          break;
        case "\\":
          out += "\\";
          break;
        case "/":
          out += "/";
          break;
        case "b":
          out += "\b";
          break;
        case "f":
          out += "\f";
          break;
        case "n":
          out += "\n";
          break;
        case "r":
          out += "\r";
          break;
        case "t":
          out += "\t";
          break;
        case "u": {
          let code = 0;
          for (let i = 0; i < 4; i++) {
            const d = advance(c);
            if (d === null) return null;
            const v = hexValue(d);
            if (v === null) return null;
            code = code * 16 + v;
          }
          out += String.fromCharCode(code);
          break;
        }
        default:
          out += esc;
      }
    } else {
      out += ch;
    }
  }
}

function hexValue(ch: string): number | null {
  const v = ch.charCodeAt(0);
  if (v >= 0x30 && v <= 0x39) return v - 0x30;
  if (v >= 0x41 && v <= 0x46) return v - 0x41 + 10;
  if (v >= 0x61 && v <= 0x66) return v - 0x61 + 10;
  return null;
}

function parseNumber(c: Cursor): ResultsNode | null {
  const start = c.pos;
  match(c, "-");
  while (current(c) !== null && /[0-9]/.test(current(c)!)) advance(c);
  if (match(c, ".")) {
    while (current(c) !== null && /[0-9]/.test(current(c)!)) advance(c);
  }
  if (match(c, "e") || match(c, "E")) {
    if (!match(c, "+")) match(c, "-");
    while (current(c) !== null && /[0-9]/.test(current(c)!)) advance(c);
  }
  const raw = c.chars.slice(start, c.pos).join("");
  if (raw.length === 0) return null;
  return { node: "scalar", type: KIND_NUMBER, text: raw };
}

export function containerNodeType(container: "object" | "array"): number {
  return container === "object" ? KIND_OBJECT : KIND_ARRAY;
}
