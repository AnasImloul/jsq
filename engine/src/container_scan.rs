//! Shared opening boilerplate for one-level container scanners.
//!
//! Every callsite that walks a container's children — the evaluator's
//! source-byte scanners, the FFI children iterator, the path
//! computation for primitive slots — opens with the same sequence:
//! resolve the parent record, verify it's a container, slice its
//! source bytes with the document mmap as the bound, compute
//! `parent_end_id` with overflow guards, and seed the skippable chain
//! head from the parent's `subtree_size`. [`ContainerOpen`] does that
//! once; per-iteration cursor work stays in the caller because each
//! scanner has its own emission shape (object key parsing, fused
//! field match, batched sink, etc.).

use crate::document::{Document, NodeKind, NodeRecord, NULL_NODE};

/// Threshold at which the engine asks the kernel to prefetch a
/// container's source bytes. Below this, the OS readahead window keeps
/// up with sequential scans; above it the per-page demand-paging
/// stalls become user-visible on cold caches.
const WILLNEED_THRESHOLD_BYTES: usize = 64 * 1024 * 1024;

/// Resolved opening state for a one-level container scan. `source`
/// covers exactly the parent's bytes, so per-iteration cursors are
/// always relative to that slice.
pub(crate) struct ContainerOpen<'a> {
    pub(crate) parent: NodeRecord,
    pub(crate) kind: NodeKind,
    pub(crate) source: &'a [u8],
    pub(crate) parent_offset: usize,
    /// One-past-the-last record id that belongs to the parent's
    /// subtree; safe to compare against without overflow on a corrupt
    /// sidecar.
    pub(crate) parent_end_id: u32,
    /// Initial value of the skippable-chain cursor; either the
    /// parent's first record-bearing descendant or `NULL_NODE` for
    /// containers whose entire subtree is primitives.
    pub(crate) initial_next_skippable: u32,
}

impl<'a> ContainerOpen<'a> {
    /// Returns `None` if the parent doesn't exist, isn't a container,
    /// or has bytes outside the source mmap.
    pub(crate) fn new(doc: &'a Document, parent_id: u32) -> Option<Self> {
        let parent = *doc.record(parent_id)?;
        Self::from_record(doc, parent_id, parent)
    }

    /// Same as `new`, but for callers that already have the parent
    /// record in hand and want to skip the second `doc.record` lookup.
    pub(crate) fn from_record(
        doc: &'a Document,
        parent_id: u32,
        parent: NodeRecord,
    ) -> Option<Self> {
        let kind = NodeKind::from_u8(parent.kind);
        if !matches!(kind, NodeKind::Object | NodeKind::Array) {
            return None;
        }
        let parent_offset = parent.offset as usize;
        let parent_length = parent.length as usize;
        let source_end = parent_offset.checked_add(parent_length)?;
        if source_end > doc.source_mmap.len() {
            return None;
        }
        let source = &doc.source_mmap[parent_offset..source_end];
        let parent_end_id = parent_id
            .checked_add(parent.subtree_size)
            .unwrap_or(doc.records().len() as u32);
        let initial_next_skippable = if parent.subtree_size > 1 {
            parent_id.checked_add(1).unwrap_or(NULL_NODE)
        } else {
            NULL_NODE
        };
        Some(Self {
            parent,
            kind,
            source,
            parent_offset,
            parent_end_id,
            initial_next_skippable,
        })
    }

    /// Closing bracket byte for the parent's kind.
    pub(crate) fn close_byte(&self) -> u8 {
        if self.kind == NodeKind::Object { b'}' } else { b']' }
    }

    /// Issues `MADV_WILLNEED` for the parent's bytes when the container
    /// is large enough that demand paging would stall the walk. No-op
    /// for smaller containers.
    pub(crate) fn issue_willneed_hint(&self, doc: &Document) {
        if self.parent.length as usize >= WILLNEED_THRESHOLD_BYTES {
            let _ = doc.source_mmap.advise_range(
                memmap2::Advice::WillNeed,
                self.parent_offset,
                self.parent.length as usize,
            );
        }
    }

    /// True iff the cursor at `pos` (relative to `source`) coincides
    /// with the offset of `next_skippable`'s record.
    pub(crate) fn at_skippable(&self, doc: &Document, pos: usize, next_skippable: u32) -> bool {
        next_skippable != NULL_NODE
            && doc
                .record(next_skippable)
                .map(|r| r.offset == (self.parent_offset + pos) as u64)
                .unwrap_or(false)
    }

    /// Advances the skippable-chain cursor by one subtree. Returns
    /// `NULL_NODE` once the chain leaves the parent's record range —
    /// also the right answer on a corrupt sidecar that would otherwise
    /// wrap into an unrelated record id.
    pub(crate) fn advance_skippable(&self, after: u32, by: u32) -> u32 {
        match after.checked_add(by) {
            Some(n) if n < self.parent_end_id => n,
            _ => NULL_NODE,
        }
    }
}
