//! App state: every open document, keyed by id behind a mutex. Each
//! engine handle is read-only after parse, so documents are shared
//! across command invocations; multiple may be open at once (one per
//! UI tab).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::bridge::DocHandle;

#[derive(Default)]
pub struct AppState {
    pub docs: Mutex<DocRegistry>,
}

/// Open documents keyed by a monotonically-increasing id the frontend
/// uses to address one tab's document. Ids are never reused, so a stale
/// id from a closed tab resolves to `None` rather than the wrong file.
#[derive(Default)]
pub struct DocRegistry {
    map: HashMap<u32, DocHandle>,
    next_id: u32,
}

impl DocRegistry {
    /// Stores `handle` under a fresh id and returns it.
    pub fn insert(&mut self, handle: DocHandle) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.map.insert(id, handle);
        id
    }

    pub fn get(&self, id: u32) -> Option<&DocHandle> {
        self.map.get(&id)
    }

    /// Drops the document (closing its engine handle), if present.
    pub fn remove(&mut self, id: u32) {
        self.map.remove(&id);
    }
}
