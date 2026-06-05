import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import type {
  Ancestor,
  ChildDto,
  CompletionContext,
  FieldSchema,
  GrammarManifest,
  NodeDetail,
  OpenResult,
  QueryRun,
  Token,
} from "./types";

export async function pickFile(): Promise<string | null> {
  const selected = await openDialog({
    multiple: false,
    directory: false,
    filters: [{ name: "JSON", extensions: ["json", "ndjson", "jsonl"] }],
  });
  return typeof selected === "string" ? selected : null;
}

export const openFile = (path: string) => invoke<OpenResult>("open", { path });
export const closeFile = (doc: number) => invoke<void>("close", { doc });

export const parseProgress = () => invoke<[number, number]>("parse_progress");

export const fetchChildren = (doc: number, parent: number, offset: number, limit: number) =>
  invoke<ChildDto[]>("children", { doc, parent, offset, limit });

export const fetchNodeDetail = (doc: number, node: number) =>
  invoke<NodeDetail>("node_detail", { doc, node });

export const fetchAncestors = (doc: number, node: number) =>
  invoke<Ancestor[]>("node_ancestors", { doc, node });

export const engineVersion = () => invoke<string>("engine_version");

export const runQuery = (doc: number, query: string, limit: number) =>
  invoke<QueryRun>("run_query", { doc, query, limit });

export const textSearch = (doc: number, needle: string, limit: number) =>
  invoke<QueryRun>("text_search", { doc, needle, limit });

export const formatQuery = (query: string) =>
  invoke<string>("format_query", { query });

export const tokenizeQuery = async (source: string): Promise<Token[]> => {
  const raw = await invoke<string>("tokenize", { source });
  try {
    return JSON.parse(raw) as Token[];
  } catch {
    return [];
  }
};

export const completionContext = async (
  source: string,
  cursor: number,
): Promise<CompletionContext | null> => {
  const raw = await invoke<string | null>("completion_context", { source, cursor });
  if (!raw) return null;
  try {
    return JSON.parse(raw) as CompletionContext;
  } catch {
    return null;
  }
};

export const grammarManifest = async (): Promise<GrammarManifest> => {
  const raw = await invoke<string>("grammar_manifest");
  return JSON.parse(raw) as GrammarManifest;
};

export const querySchema = (doc: number, query: string, limit: number) =>
  invoke<FieldSchema>("query_schema", { doc, query, limit });

export type ExportFormat = "json" | "ndjson" | "csv";

const EXPORT_META: Record<ExportFormat, { name: string; ext: string; label: string }> = {
  json: { name: "results.json", ext: "json", label: "JSON" },
  ndjson: { name: "results.ndjson", ext: "ndjson", label: "NDJSON" },
  csv: { name: "results.csv", ext: "csv", label: "CSV" },
};

/// Opens the native save panel for `format`, returning the chosen path
/// (or null if cancelled).
export async function pickExportPath(format: ExportFormat): Promise<string | null> {
  const meta = EXPORT_META[format];
  const path = await saveDialog({
    defaultPath: meta.name,
    filters: [{ name: meta.label, extensions: [meta.ext] }],
  });
  return path ?? null;
}

export const exportQuery = (
  doc: number,
  query: string,
  limit: number,
  format: ExportFormat,
  path: string,
) => invoke<void>("export_query", { doc, query, limit, format, path });
