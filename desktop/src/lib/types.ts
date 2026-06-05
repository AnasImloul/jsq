export interface OpenResult {
  docId: number;
  fileSize: number;
  totalNodeCount: number;
  rootId: number;
  loadedFromSidecar: boolean;
}

export interface ChildDto {
  id: number | null;
  kind: number;
  key: string | null;
  index: number | null;
  childCount: number;
  isContainer: boolean;
  preview: string;
  truncated: boolean;
}

export interface NodeDetail {
  id: number;
  kind: number;
  childCount: number;
  isContainer: boolean;
  byteOffset: number;
  byteLength: number;
  path: string;
  value: string | null;
  key: string | null;
  arrayIndex: number | null;
}

export interface Ancestor {
  id: number;
  label: string;
}

export interface RowDto {
  nodeId: number | null;
  kind: number;
  path: string;
  preview: string;
  fullText: string;
  childCount: number;
}

export interface TableCell {
  kind: number;
  isContainer: boolean;
  text: string | null;
  count: number | null;
}

export interface TableRow {
  nodeId: number | null;
  label: string;
  cells: Record<string, TableCell>;
}

export interface TableSnapshot {
  columns: string[];
  rows: TableRow[];
  isTabular: boolean;
}

export interface QueryRun {
  rows: RowDto[];
  hitLimit: boolean;
  scannedRows: number;
  scannedBytes: number;
  lookupCalls: number;
  missingIndex: [string, string] | null;
  table: TableSnapshot;
}

export interface Token {
  category: string;
  offset: number;
  length: number;
}

export type CompletionMode = "fieldAccess" | "valueStart" | "afterExpression";

export interface CompletionContext {
  mode: CompletionMode;
  partial: string;
  partialUtf16Length: number;
  contextQuery?: string;
}

export interface FieldSchema {
  kinds: number;
  keys: string[];
}

export interface ManifestKeyword {
  text: string;
  category: string;
  role: string;
}

export interface GrammarManifest {
  keywords: ManifestKeyword[];
  operators: { text: string; kind: string }[];
  punctuation: { text: string; kind: string }[];
}

export const KIND_NULL = 0;
export const KIND_BOOL = 1;
export const KIND_NUMBER = 2;
export const KIND_STRING = 3;
export const KIND_ARRAY = 4;
export const KIND_OBJECT = 5;

interface KindMeta {
  label: string;
  badge: string;
  short: string;
  color: string;
}

const KINDS: Record<number, KindMeta> = {
  [KIND_NULL]: { label: "Null", badge: "null", short: "NULL", color: "#8e8e93" },
  [KIND_BOOL]: { label: "Boolean", badge: "T/F", short: "BOOL", color: "#ff9500" },
  [KIND_NUMBER]: { label: "Number", badge: "123", short: "NUM", color: "#af52de" },
  [KIND_STRING]: { label: "String", badge: "abc", short: "STR", color: "#007aff" },
  [KIND_ARRAY]: { label: "Array", badge: "[]", short: "ARR", color: "#30b0c7" },
  [KIND_OBJECT]: { label: "Object", badge: "{}", short: "OBJ", color: "#5856d6" },
};

export function kindMeta(kind: number): KindMeta {
  return KINDS[kind] ?? KINDS[KIND_NULL];
}

export function isContainerKind(kind: number): boolean {
  return kind === KIND_ARRAY || kind === KIND_OBJECT;
}
