// BigJSON engine — C header for the Rust static library. Hand-written.

#ifndef BIGJSON_ENGINE_H
#define BIGJSON_ENGINE_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque document handle.
typedef struct EngineDocument EngineDocument;

// Sentinel for "no node" (e.g. root's parent, leaf's first_child).
#define ENGINE_NODE_NONE ((uint32_t)0xFFFFFFFFu)

// Node kind codes returned by engine_node_kind.
#define ENGINE_KIND_NULL    0
#define ENGINE_KIND_BOOL    1
#define ENGINE_KIND_NUMBER  2
#define ENGINE_KIND_STRING  3
#define ENGINE_KIND_ARRAY   4
#define ENGINE_KIND_OBJECT  5

// Slice borrowed from the document. Valid until the document is closed.
// `length` is uint64_t so multi-GB spans (e.g. the root container of a
// 10GB document) round-trip without truncation.
typedef struct {
    const uint8_t *data;
    uint64_t length;
} EngineSlice;

// Slice owned by the caller. Free with engine_free_owned_bytes.
// `length` is uint64_t so >4 GiB outputs (large query results, big
// jq paths) free with the correct allocator layout — a uint32_t
// truncation here would mismatch the original Box allocation and
// cause undefined behaviour in the global allocator.
typedef struct {
    uint8_t *data;
    uint64_t length;
} EngineOwnedBytes;

// Returns a static, NUL-terminated semver string.
const char *engine_version(void);

// Opens a JSON file by absolute path. If `index_dir` is non-NULL, the engine
// caches its index there as a `.jsonidx` sidecar (validated by source size
// and mtime); subsequent opens of an unchanged source are cache hits and
// avoid re-parsing. Pass NULL to disable caching.
// Returns NULL on failure; check engine_last_error().
EngineDocument *engine_open(const char *source_path, const char *index_dir);

// Returns 1 if the document was loaded from a sidecar (cache hit), 0 if it
// was parsed from source this session.
uint8_t engine_loaded_from_sidecar(const EngineDocument *doc);

// Releases all resources associated with the document.
void engine_close(EngineDocument *doc);

// Thread-local pointer to the last error message. Valid until the next
// engine call on this thread that fails.
const char *engine_last_error(void);

// Inventory.
uint64_t engine_total_node_count(const EngineDocument *doc);
uint64_t engine_file_size(const EngineDocument *doc);
uint32_t engine_root(const EngineDocument *doc);

// Per-node metadata. Returns ENGINE_NODE_NONE for missing IDs / sentinels.
uint8_t  engine_node_kind(const EngineDocument *doc, uint32_t node);
uint32_t engine_node_parent(const EngineDocument *doc, uint32_t node);
uint32_t engine_node_first_child(const EngineDocument *doc, uint32_t node);
uint32_t engine_node_next_sibling(const EngineDocument *doc, uint32_t node);
uint32_t engine_node_child_count(const EngineDocument *doc, uint32_t node);

// Fills `out_ids` with up to `max` child IDs starting from the `offset`-th
// child of `parent`. Returns the number actually written. The caller owns
// the `out_ids` buffer (must hold at least `max` uint32_t entries).
uint32_t engine_node_children_batch(
    const EngineDocument *doc,
    uint32_t parent,
    uint32_t offset,
    uint32_t max,
    uint32_t *out_ids
);

// One row's worth of tree metadata, packed for batch transfer.
//
// Identification:
//   id != ENGINE_NODE_NONE  -> record-bearing child (container or fat string);
//                              call engine_node_* with this id.
//   id == ENGINE_NODE_NONE  -> primitive child (no record under the hybrid
//                              emit-gate); use the inline fields below.
//
// Object member keys (flags & FLAG_OBJECT_MEMBER):
//   flags & FLAG_KEY_IN_SOURCE == 0 -> key_offset is into the document's
//                                      decoded keys arena (UTF-8).
//   flags & FLAG_KEY_IN_SOURCE != 0 -> key_offset is into the source mmap;
//                                      the bytes are raw (between the JSON
//                                      string's quotes, escapes intact).
//
// Values:
//   value_offset / value_length always describe a slice of the source mmap.
#define FLAG_OBJECT_MEMBER 0x01
#define FLAG_ARRAY_ELEMENT 0x02
#define FLAG_KEY_IN_SOURCE 0x04

typedef struct {
    uint32_t id;
    uint8_t kind;
    uint8_t flags;
    uint16_t _pad;
    uint32_t child_count;
    uint64_t key_offset;
    uint32_t key_length;
    uint32_t array_index;
    uint64_t value_offset;
    // 64-bit so multi-GB values (e.g. the root of a 10GB document) don't
    // truncate. Replaces the old (uint32_t value_length, uint32_t _pad2)
    // pair; struct size and field offsets are unchanged.
    uint64_t value_length;
} EngineChildMeta;

// Returns metadata for up to `max` children of `parent` starting at
// `offset`. Re-scans from the first child every call — paginating
// through huge containers (>>10K children) with this is quadratic.
// Use engine_node_children_meta_batch_resume below for those cases.
uint32_t engine_node_children_meta_batch(
    const EngineDocument *doc,
    uint32_t parent,
    uint32_t offset,
    uint32_t max,
    EngineChildMeta *out
);

// Resumable scan state. Caller initialises pos=ENGINE_SCAN_STATE_FRESH
// and next_skippable=ENGINE_NODE_NONE; the FFI updates the struct on
// every call so the next call resumes where the previous one stopped.
// `pos` is 64-bit so byte offsets in multi-GB files don't truncate.
#define ENGINE_SCAN_STATE_FRESH ((uint64_t)0xFFFFFFFFFFFFFFFFull)
typedef struct {
    uint64_t pos;
    uint32_t next_skippable;
    uint32_t array_index;
} EngineScanState;

// Stateful counterpart to engine_node_children_meta_batch. Iterates
// `parent`'s children in O(source_bytes) total instead of O(children²).
// First call: state = { ENGINE_SCAN_STATE_FRESH, ENGINE_NODE_NONE, 0 }.
// Subsequent calls pass the same state struct back in — its contents
// are opaque. Returns the number of entries written; 0 once the
// parent's children are exhausted.
uint32_t engine_node_children_meta_batch_resume(
    const EngineDocument *doc,
    uint32_t parent,
    EngineScanState *state,
    uint32_t max,
    EngineChildMeta *out
);

// Byte position of the node's value within the source file. Useful for
// inspector metadata; raw access to the underlying mmap goes through
// engine_node_value_bytes instead.
uint64_t engine_node_byte_offset(const EngineDocument *doc, uint32_t node);
uint64_t engine_node_byte_length(const EngineDocument *doc, uint32_t node);

// Raw bytes for the node's value as it appears in the source file (e.g.
// with surrounding quotes for strings; with the literal text for numbers).
EngineSlice engine_node_value_bytes(const EngineDocument *doc, uint32_t node);

// Slice into the document's source mmap (source_flag != 0) or its decoded
// keys arena (source_flag == 0). Used to read primitive children's key /
// value bytes via the offsets carried in EngineChildMeta. The returned
// pointer is valid for the document's lifetime.
EngineSlice engine_node_value_bytes_at(
    const EngineDocument *doc,
    uint64_t offset,
    uint64_t length,
    uint8_t source_flag
);

// Decoded UTF-8 key for an object child; empty for non-object children.
EngineSlice engine_node_key(const EngineDocument *doc, uint32_t node);

// Array index for an array child; 0 for non-array children — caller should
// gate on engine_node_is_array_element first.
uint32_t engine_node_array_index(const EngineDocument *doc, uint32_t node);

uint8_t engine_node_is_array_element(const EngineDocument *doc, uint32_t node);
uint8_t engine_node_is_object_member(const EngineDocument *doc, uint32_t node);

// jq-style path string (UTF-8, no NUL terminator). Caller must free with
// engine_free_owned_bytes.
EngineOwnedBytes engine_node_path(const EngineDocument *doc, uint32_t node);

// jq-style path for the slot-th child of `parent`. Works uniformly for
// record-bearing and primitive children; the engine decodes raw key
// bytes itself, so callers don't reimplement segment formatting or
// escape handling. {NULL, 0} on a non-container parent or out-of-range
// slot. Caller-owned bytes; pair with engine_free_owned_bytes.
EngineOwnedBytes engine_node_child_path(
    const EngineDocument *doc, uint32_t parent, uint32_t slot
);

// Decodes a JSON-string byte span (the bytes between the surrounding
// quotes — NOT including them) into UTF-8. Mirrors the decoder used
// internally for keys and values. {NULL, 0} on a malformed escape
// sequence or null input. Caller-owned bytes; pair with
// engine_free_owned_bytes.
EngineOwnedBytes engine_decode_json_string(const uint8_t *data, uint64_t length);

// Frees buffers handed out by engine_node_path (and any future _owned APIs).
void engine_free_owned_bytes(EngineOwnedBytes bytes);

// ===== Grammar manifest =====

// Single source of truth for the surface query language vocabulary,
// returned as UTF-8 JSON. Stable schema:
//   {
//     "keywords":    [{"text": "where", "category": "clause", "role": "afterExpression"}, ...],
//     "operators":   [{"text": "==", "kind": "eq"}, ...],
//     "punctuation": [{"text": ".",  "kind": "dot"}, ...]
//   }
// Both the Swift highlighter (token category lookup) and autocomplete
// (suggestion lists by role) consume this. Caller-owned bytes; pair
// with engine_free_owned_bytes.
EngineOwnedBytes engine_grammar_manifest(void);

// Tokenises a query source string for the UI highlighter. Returns a
// JSON array of `{"category": "...", "offset": N, "length": M}` triples,
// where offsets and lengths are in UTF-16 code units (NSString-compatible).
// Forgiving — unrecognised bytes surface as "error" tokens; never errors.
// Caller-owned bytes; pair with engine_free_owned_bytes. {NULL, 0} on
// null / non-UTF-8 input.
EngineOwnedBytes engine_tokenize(const char *source);

// Cursor-aware autocomplete classifier. Given `(source, cursor_utf16)`,
// returns a JSON object describing what completion makes sense at the
// cursor:
//   {
//     "mode": "fieldAccess" | "valueStart" | "afterExpression",
//     "partial": "...",
//     "partialUtf16Length": N,
//     "contextQuery": "..."   // only when mode == "fieldAccess"
//   }
// Returns {NULL, 0} when the cursor is mid-token in a position that
// doesn't admit completions, or on null / non-UTF-8 input. Caller-owned
// bytes; pair with engine_free_owned_bytes.
EngineOwnedBytes engine_completion_context(const char *source, uint32_t cursor_utf16);

// ===== Query =====

typedef struct EngineQueryResults EngineQueryResults;

typedef struct {
    uint32_t node_id;        // ENGINE_NODE_NONE for synthetic
    uint8_t kind;
    uint8_t _pad[3];
    EngineSlice path;
    EngineSlice preview;
    EngineSlice full_text;
} EngineQueryResultView;

// Last parse error (thread-local). Pointers valid until the next call on
// this thread that fails parsing.
const char *engine_query_last_parse_error(void);
uint32_t engine_query_last_parse_error_position(void);

// Parses and runs the query. Returns NULL on parse error; check
// engine_query_last_parse_error and _position. Otherwise returns an opaque
// results handle that must be freed with engine_query_results_free.
EngineQueryResults *engine_query_run(
    const EngineDocument *doc,
    const char *query,
    uint32_t limit
);

// Plain-text substring search across the whole document. Emits any node
// whose object key OR primitive value contains `needle` (case-insensitive
// ASCII). Returns NULL only on a null document or empty needle.
EngineQueryResults *engine_query_text_search(
    const EngineDocument *doc,
    const char *needle,
    uint32_t limit
);

void engine_query_results_free(EngineQueryResults *results);

uint32_t engine_query_results_count(const EngineQueryResults *results);
uint8_t engine_query_results_hit_limit(const EngineQueryResults *results);

// Rows the source path emitted before the rest of the pipeline ran —
// the "scanned rows" stat surfaced in the UI popover. Zero on a null
// `results` handle.
uint64_t engine_query_results_scanned_rows(const EngineQueryResults *results);

// Successful `lookup(...)` invocations during this query — useful for
// diagnosing high-fanout field-sets. Zero on a null `results` handle.
uint64_t engine_query_results_lookup_calls(const EngineQueryResults *results);

// Sum of source byte spans for every node the source path emitted.
// Compare against the document's file size to spot memory-bandwidth-
// bound queries — when this approaches the file size the engine read
// most of the document regardless of how many rows survived later
// filtering. Zero on a null `results` handle.
uint64_t engine_query_results_scanned_bytes(const EngineQueryResults *results);

// Re-formats `query` with canonical indentation. Returns owned UTF-8
// bytes; pair with engine_free_owned_bytes. {NULL, 0} on parse error
// (engine_query_last_parse_error() carries the message).
EngineOwnedBytes engine_format_query(const char *query);

// Render formatters — produce owned UTF-8 bytes from a result set.
// Both the macOS export menu and the jsq CLI delegate here so the
// result-row → bytes transformation lives in exactly one place.
EngineOwnedBytes engine_render_ndjson(
    const EngineQueryResults *results,
    const EngineDocument *doc
);
EngineOwnedBytes engine_render_json_array(
    const EngineQueryResults *results,
    const EngineDocument *doc
);
EngineOwnedBytes engine_render_csv(const EngineQueryResults *results);

// One-shot helper that runs `query` against `doc` and renders the
// result set in `format` (0=ndjson, 1=json_array, 2=csv). Used by
// the export menu, which doesn't hold a results handle long enough
// to call the per-format renderers separately.
EngineOwnedBytes engine_query_run_and_render(
    const EngineDocument *doc,
    const char *query,
    uint32_t limit,
    uint8_t format
);

// Writes the current parse-progress to *parsed and *total (either may
// be NULL). `total` is zero before any document has started loading;
// while an open is in flight on another thread, parsed/total tracks
// the streaming parser's progress through the source bytes.
void engine_current_parse_progress(uint64_t *parsed, uint64_t *total);

// Returns a view of the i-th result. The slices in the view point into
// the EngineQueryResults storage and remain valid until that handle is
// freed.
EngineQueryResultView engine_query_results_at(
    const EngineQueryResults *results,
    uint32_t idx
);

// Walks every child of `node` once and writes per-kind counts (indexed
// by ENGINE_KIND_*) into `out_counts` (length 6). Returns the total
// number of children. Lets callers render an exact type histogram for
// an object/array without fetching every child's metadata.
uint32_t engine_node_children_kind_counts(
    const EngineDocument *doc,
    uint32_t node,
    uint32_t *out_counts
);

// Bitmask of kinds produced by the query (sampled up to `limit` outputs).
// Bits: 0=null, 1=bool, 2=number, 3=string, 4=array, 5=object.
// Returns 0 on parse error or no outputs. Used to decide whether to
// suggest object keys or array accessors at a given autocomplete position.
uint8_t engine_kinds_for_query(
    const EngineDocument *doc,
    const char *query,
    uint32_t limit
);

// Schema-aware autocomplete. Runs `query` and returns the union of object
// keys among the first `limit` outputs as a JSON array (UTF-8 bytes,
// caller-owned, free with engine_free_owned_bytes). Returns {NULL, 0} on
// parse error or when no object outputs are produced.
EngineOwnedBytes engine_keys_for_query(
    const EngineDocument *doc,
    const char *query,
    uint32_t limit
);

// ===== Foreign-key indexes =====

// Stats returned by engine_query_create_index. `ok` is 1 on success,
// 0 on a parse error (in which case engine_query_last_parse_error has
// the message).
typedef struct {
    uint8_t ok;
    uint8_t _pad[7];
    uint64_t source_count;
    uint64_t indexed_count;
    uint64_t bucket_count;
    uint64_t approx_bytes;
} EngineIndexStats;

// Builds and registers a foreign-key index on (source_expr, key_expr).
// Subsequent `lookup(source_expr; key_expr)` calls become O(1). Re-running
// rebuilds in place.
EngineIndexStats engine_query_create_index(
    const EngineDocument *doc,
    const char *source_expr,
    const char *key_expr
);

// Drops the index for (source_canon, key_canon). Both arguments must be
// the canonical expression form (typically supplied by list_indexes or
// by the missing-index fields on QueryResults). Returns 1 if dropped,
// 0 if no such index existed.
uint8_t engine_query_drop_index(
    const EngineDocument *doc,
    const char *source_canon,
    const char *key_canon
);

// JSON array of registered indexes. Caller-owned; free with
// engine_free_owned_bytes. Each element:
//   {"source": "...", "key": "...", "source_count": N, "indexed_count": M,
//    "bucket_count": K, "approx_bytes": B}
EngineOwnedBytes engine_query_list_indexes(const EngineDocument *doc);

// When evaluation hit `lookup` with no matching index, these return the
// canonical SOURCE / KEY expression strings. NULL on no error. Pointers
// remain valid until engine_query_results_free is called.
const char *engine_query_results_missing_index_source(const EngineQueryResults *results);
const char *engine_query_results_missing_index_key(const EngineQueryResults *results);

#ifdef __cplusplus
}
#endif

#endif // BIGJSON_ENGINE_H
