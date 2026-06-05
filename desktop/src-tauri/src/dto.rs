//! Serde structs returned to the frontend. Field names use camelCase to
//! match the TypeScript side.

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub doc_id: u32,
    pub file_size: u64,
    pub total_node_count: u64,
    pub root_id: u32,
    pub loaded_from_sidecar: bool,
}

/// One row in the tree view. `id` is `null` for primitive children
/// (which have no record and can't be expanded). Containers carry an
/// `id` and a `childCount`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildDto {
    pub id: Option<u32>,
    pub kind: u8,
    pub key: Option<String>,
    pub index: Option<u32>,
    pub child_count: u32,
    pub is_container: bool,
    pub preview: String,
    pub truncated: bool,
}

/// Full detail for the currently-selected node, driving the inspector
/// header, value, and metadata sections. Only record-bearing nodes
/// (containers and fat strings) are selectable, so `id` is always valid.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDetailDto {
    pub id: u32,
    pub kind: u8,
    pub child_count: u32,
    pub is_container: bool,
    pub byte_offset: u64,
    pub byte_length: u64,
    pub path: String,
    pub value: Option<String>,
    pub key: Option<String>,
    pub array_index: Option<u32>,
}

/// One clickable breadcrumb segment, root → leaf.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AncestorDto {
    pub id: u32,
    pub label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDto {
    pub node_id: Option<u32>,
    pub kind: u8,
    pub path: String,
    pub preview: String,
    pub full_text: String,
    /// Child count for document-backed container rows (0 otherwise).
    /// Synthetic containers compute their own count from `full_text` on
    /// the frontend.
    pub child_count: u32,
}

/// One cell in the tabular projection. Scalars carry `text`; containers
/// carry a `count` and render as a `{ N }` / `[ N ]` chip.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TableCellDto {
    pub kind: u8,
    pub is_container: bool,
    pub text: Option<String>,
    pub count: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRowDto {
    pub node_id: Option<u32>,
    pub label: String,
    pub cells: std::collections::HashMap<String, TableCellDto>,
}

/// Spreadsheet projection of a result set: union of top-level keys as
/// columns (frequency-ordered, capped), one row per result. `isTabular`
/// drives the frontend's auto-mode selection.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TableSnapshotDto {
    pub columns: Vec<String>,
    pub rows: Vec<TableRowDto>,
    pub is_tabular: bool,
}

/// Sampled schema at an autocomplete field-access scope: the union of
/// kinds (bitmask) and object keys produced by the context query.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSchema {
    pub kinds: u8,
    pub keys: Vec<String>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryRunDto {
    pub rows: Vec<RowDto>,
    pub hit_limit: bool,
    pub scanned_rows: u64,
    pub scanned_bytes: u64,
    pub lookup_calls: u64,
    /// `(source, key)` of a missing foreign-key index, when the query
    /// referenced a `lookup(...)` with no matching index.
    pub missing_index: Option<(String, String)>,
    /// Tabular projection of `rows`, built in-process so the frontend
    /// can offer table view without per-row IPC round-trips.
    pub table: TableSnapshotDto,
}
