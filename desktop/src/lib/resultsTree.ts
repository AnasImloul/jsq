import { containerNodeType, parseResultsJSON, type ResultsNode } from "./resultsJson";
import { KIND_OBJECT, isContainerKind, type ChildDto, type RowDto } from "./types";

/// Separator for path segments. Non-printing so it can't collide with a
/// real key. Paths are the expansion-set / lazy-state keys.
const SEP = "";

export type RowMode = { kind: "named"; name: string } | { kind: "indexed"; index: number };

export type ChildrenSource =
  | { kind: "eager"; entries: [string, ResultsNode][] }
  | { kind: "lazy"; nodeId: number; total: number };

export type RowPayload =
  | { kind: "scalar"; text: string }
  | { kind: "container"; container: "object" | "array"; source: ChildrenSource };

export interface RowEntry {
  mode: RowMode;
  type: number;
  payload: RowPayload;
  nodeId: number | null;
  path: string;
}

/// Whether a row's path reads as a user-meaningful name (aggregate output
/// name or bucket key) rather than a stream path/index.
function labelLooksNamed(r: RowDto): boolean {
  if (r.nodeId !== null) return false;
  if (r.path.length === 0) return false;
  if (r.path.startsWith("(synthetic)")) return false;
  if (r.path.includes(".") || r.path.includes("[")) return false;
  return true;
}

export function buildTopRows(rows: RowDto[]): RowEntry[] {
  return rows.map((r, idx) => entryForResult(r, idx));
}

function entryForResult(r: RowDto, index: number): RowEntry {
  const named = labelLooksNamed(r);
  const mode: RowMode = named ? { kind: "named", name: r.path } : { kind: "indexed", index };
  const path = named ? r.path : `[${index}]`;

  if (r.nodeId !== null) {
    if (isContainerKind(r.kind)) {
      return {
        mode,
        type: r.kind,
        payload: {
          kind: "container",
          container: r.kind === KIND_OBJECT ? "object" : "array",
          source: { kind: "lazy", nodeId: r.nodeId, total: r.childCount },
        },
        nodeId: r.nodeId,
        path,
      };
    }
    return {
      mode,
      type: r.kind,
      payload: { kind: "scalar", text: r.fullText || r.preview },
      nodeId: r.nodeId,
      path,
    };
  }

  const text = r.fullText || r.preview;
  const parsed = parseResultsJSON(text);
  if (parsed) {
    return entryForEager(parsed, mode, path);
  }
  return { mode, type: r.kind, payload: { kind: "scalar", text }, nodeId: null, path };
}

export function entryForEager(node: ResultsNode, mode: RowMode, path: string): RowEntry {
  if (node.node === "scalar") {
    return { mode, type: node.type, payload: { kind: "scalar", text: node.text }, nodeId: null, path };
  }
  return {
    mode,
    type: containerNodeType(node.container),
    payload: { kind: "container", container: node.container, source: { kind: "eager", entries: node.entries } },
    nodeId: null,
    path,
  };
}

/// Children of an expanded eager (synthetic) container.
export function eagerChildren(
  parentPath: string,
  container: "object" | "array",
  entries: [string, ResultsNode][],
): RowEntry[] {
  return entries.map(([label, node], idx) => {
    const named = container === "object";
    const mode: RowMode = named ? { kind: "named", name: label } : { kind: "indexed", index: idx };
    const seg = named ? label : `[${idx}]`;
    return entryForEager(node, mode, parentPath + SEP + seg);
  });
}

/// One row entry for a document-backed lazy child.
export function entryForChild(child: ChildDto, idx: number, parentPath: string): RowEntry {
  const isNamed = child.key !== null;
  const arrayIndex = child.index ?? idx;
  const mode: RowMode = isNamed
    ? { kind: "named", name: child.key! }
    : { kind: "indexed", index: arrayIndex };
  const seg = isNamed ? child.key! : `[${arrayIndex}]`;
  const path = parentPath + SEP + seg;

  if (child.isContainer && child.id !== null) {
    return {
      mode,
      type: child.kind,
      payload: {
        kind: "container",
        container: child.kind === KIND_OBJECT ? "object" : "array",
        source: { kind: "lazy", nodeId: child.id, total: child.childCount },
      },
      nodeId: child.id,
      path,
    };
  }
  return {
    mode,
    type: child.kind,
    payload: { kind: "scalar", text: child.preview + (child.truncated ? "…" : "") },
    nodeId: child.id,
    path,
  };
}

export function containerCount(source: ChildrenSource): number {
  return source.kind === "eager" ? source.entries.length : source.total;
}

export function containerSummary(container: "object" | "array", count: number): string {
  if (container === "object") {
    return `{ ${count} ${count === 1 ? "key" : "keys"} }`;
  }
  return `[ ${count} ${count === 1 ? "item" : "items"} ]`;
}
