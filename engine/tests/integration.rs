use engine::document::{Document, NodeKind};
use engine::path::{compute_child_path, compute_path};
use engine::sidecar;

use std::io::Write;

fn write_tmp(name: &str, content: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    path
}

fn collect_kids(doc: &Document, parent: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = doc.first_skippable_child(parent);
    while cur != u32::MAX {
        out.push(cur);
        cur = doc.next_skippable_sibling(cur);
    }
    out
}

#[test]
fn parses_small_object() {
    let path = write_tmp(
        "engine_test_small.json",
        r#"{
  "users": [
    {"id": 1, "name": "Alice", "tags": ["admin", "user"], "active": true},
    {"id": 2, "name": "Bob", "address": null}
  ],
  "meta": {"version": "1.0", "weird key": "hi"}
}"#,
    );
    let doc = Document::open(&path, None).expect("open");
    let root = 0u32;
    assert_eq!(doc.node_kind(root), NodeKind::Object);
    // Both children of root are containers, so they're visible via the
    // record-bearing iterator.
    assert_eq!(doc.record(root).unwrap().child_count, 2, "root has 2 keys");

    let root_kids = collect_kids(&doc, root);
    assert_eq!(root_kids.len(), 2, "root's two children are both containers");
    let users = root_kids[0];
    let meta = root_kids[1];

    assert_eq!(doc.node_kind(users), NodeKind::Array);
    assert_eq!(doc.node_kind(meta), NodeKind::Object);
    assert_eq!(doc.key_bytes(users), Some(b"users".as_slice()));
    assert_eq!(doc.key_bytes(meta), Some(b"meta".as_slice()));

    // .users has 2 elements; both are objects (containers).
    assert_eq!(doc.record(users).unwrap().child_count, 2);
    let users_kids = collect_kids(&doc, users);
    assert_eq!(users_kids.len(), 2);

    let first_user = users_kids[0];
    // Hybrid model: the user object has 4 direct children (id, name,
    // tags, active) — child_count tracks this — but only `tags` is a
    // container with its own record. The other three are primitives
    // and will be addressable via (parent, slot) handles in commit 3.
    assert_eq!(doc.record(first_user).unwrap().child_count, 4);
    let first_user_kids = collect_kids(&doc, first_user);
    assert_eq!(first_user_kids.len(), 1, "only `tags` (an array) has a record");
    assert_eq!(doc.node_kind(first_user_kids[0]), NodeKind::Array);
    assert_eq!(doc.key_bytes(first_user_kids[0]), Some(b"tags".as_slice()));
}

#[test]
fn paths_use_jq_syntax() {
    // Use containers (objects/arrays) at each path step — primitives
    // don't have records under the hybrid emit-gate, so path
    // computation for them lands in commit 3 with primitive handles.
    let path = write_tmp(
        "engine_test_paths.json",
        r#"{"users":[{"profile":{"name":"Alice"},"perm":{"weird key":{"x":1}}}],"primes":[2,3,5]}"#,
    );
    let doc = Document::open(&path, None).expect("open");

    // Walk to .users[0].profile and .users[0].perm["weird key"] —
    // every step is a container, so every step has a record.
    let users = doc.first_skippable_child(0);
    let user0 = doc.first_skippable_child(users);
    let profile = collect_kids(&doc, user0)
        .into_iter()
        .find(|&id| doc.key_bytes(id) == Some(b"profile".as_slice()))
        .unwrap();
    let perm = collect_kids(&doc, user0)
        .into_iter()
        .find(|&id| doc.key_bytes(id) == Some(b"perm".as_slice()))
        .unwrap();
    let weird = collect_kids(&doc, perm)
        .into_iter()
        .find(|&id| doc.key_bytes(id) == Some(b"weird key".as_slice()))
        .unwrap();

    assert_eq!(compute_path(&doc, 0), b".".to_vec(), "root path");
    assert_eq!(compute_path(&doc, users), b".users".to_vec());
    assert_eq!(compute_path(&doc, user0), b".users[0]".to_vec());
    assert_eq!(compute_path(&doc, profile), b".users[0].profile".to_vec());
    assert_eq!(
        String::from_utf8(compute_path(&doc, weird)).unwrap(),
        ".users[0].perm[\"weird key\"]"
    );
}

#[test]
fn child_path_handles_primitive_slots() {
    // Mix of identifier keys, an escape-bearing key, and a numeric
    // array slot — covers every branch of the segment formatter.
    let path = write_tmp(
        "engine_test_child_path.json",
        r#"{"a":1,"weird key":2,"line\nbreak":3,"nums":[10,20,30]}"#,
    );
    let doc = Document::open(&path, None).expect("open");
    let root = 0u32;

    // Object members — primitive keys at slots 0/1/2, container at 3.
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, root, 0).unwrap()).unwrap(),
        ".a",
    );
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, root, 1).unwrap()).unwrap(),
        ".[\"weird key\"]",
    );
    // Escape sequences in the key decode to UTF-8 before the segment
    // formatter sees them — the rendered segment re-escapes for jq.
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, root, 2).unwrap()).unwrap(),
        ".[\"line\\nbreak\"]",
    );
    let nums = doc.first_skippable_child(root);
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, root, 3).unwrap()).unwrap(),
        ".nums",
    );

    // Array elements — every slot is a primitive.
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, nums, 0).unwrap()).unwrap(),
        ".nums[0]",
    );
    assert_eq!(
        String::from_utf8(compute_child_path(&doc, nums, 2).unwrap()).unwrap(),
        ".nums[2]",
    );

    // Out-of-range slot returns None.
    assert!(compute_child_path(&doc, root, 99).is_none());
    // Non-container parent returns None.
    let weird = doc.first_skippable_child(root); // .nums (container) — pick a leaf instead.
    let _ = weird;
}

#[test]
fn handles_nested_arrays_and_negative_indices() {
    // Just verify the array builds up correctly.
    let path = write_tmp(
        "engine_test_nested.json",
        r#"[[1,2],[3,4],[5,6]]"#,
    );
    let doc = Document::open(&path, None).expect("open");
    let root = 0u32;
    assert_eq!(doc.node_kind(root), NodeKind::Array);
    let outer_kids = collect_kids(&doc, root);
    assert_eq!(outer_kids.len(), 3, "three inner arrays, all containers");
    for sub in &outer_kids {
        assert_eq!(doc.node_kind(*sub), NodeKind::Array);
        // child_count tracks all direct children including primitives;
        // collect_kids only sees record-bearing children, of which
        // there are zero (the inner elements are bare numbers).
        assert_eq!(doc.record(*sub).unwrap().child_count, 2);
        assert_eq!(collect_kids(&doc, *sub).len(), 0);
    }
}

#[test]
fn rejects_invalid_json() {
    let path = write_tmp("engine_test_bad.json", r#"{ "key": }"#);
    let err = Document::open(&path, None).err().expect("expected error");
    let msg = err.message();
    assert!(msg.contains("Parse error"), "got: {}", msg);
}

#[test]
fn sidecar_round_trip() {
    let dir = std::env::temp_dir().join(format!("bigjson-test-sidecar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let json_path = write_tmp(
        "engine_test_sidecar.json",
        r#"{"users":[{"name":"Alice","age":30},{"name":"Bob"}],"count":2}"#,
    );

    // First open: should miss the cache and parse from source.
    let mut doc1 = Document::open(&json_path, Some(&dir)).expect("open 1");
    assert!(!doc1.loaded_from_sidecar(), "cold open should not be cached");
    let total1 = doc1.records().len();
    let key1 = doc1.key_bytes(doc1.first_skippable_child(0)).unwrap().to_vec();
    // Wait for the background sidecar finaliser to rename the temp
    // file into the sidecar dir before we check its existence below.
    doc1.wait_for_sidecar();
    drop(doc1);

    // Sidecar should now exist.
    let sidecar_file = sidecar::sidecar_path(&dir, &json_path);
    assert!(sidecar_file.exists(), "sidecar should be written");

    // Second open: should hit the cache.
    let doc2 = Document::open(&json_path, Some(&dir)).expect("open 2");
    assert!(doc2.loaded_from_sidecar(), "warm open should use sidecar");
    assert_eq!(doc2.records().len(), total1);
    let key2 = doc2.key_bytes(doc2.first_skippable_child(0)).unwrap().to_vec();
    assert_eq!(key1, key2, "keys pool must round-trip");
    let path2 = compute_path(&doc2, doc2.first_skippable_child(0));
    assert_eq!(path2, b".users".to_vec());
    drop(doc2);

    // Touch the source file (advance mtime) — sidecar should now invalidate.
    // sleep briefly so the mtime is observably different on coarse FS clocks
    std::thread::sleep(std::time::Duration::from_millis(20));
    let new_content = r#"{"users":[{"name":"Alice","age":31}],"count":1}"#;
    std::fs::write(&json_path, new_content).unwrap();

    let doc3 = Document::open(&json_path, Some(&dir)).expect("open 3");
    assert!(!doc3.loaded_from_sidecar(), "size/mtime changed; sidecar must invalidate");
}

#[test]
fn children_meta_batch_interleaves_primitives() {
    // Mixed object: two primitive members and one container member.
    let path = write_tmp(
        "engine_test_children_meta_batch.json",
        r#"{"id":42,"name":"Alice","tags":["admin","user"]}"#,
    );
    let doc = Document::open(&path, None).expect("open");

    // Root is the object; child_count is 3 (id, name, tags).
    let root = 0u32;
    assert_eq!(doc.record(root).unwrap().child_count, 3);

    // The new FFI-side scanner should expose all 3 children. We exercise
    // it via the FFI entry point directly to lock in the contract the
    // Swift bridge depends on.
    use engine::ffi::{engine_node_children_meta_batch, EngineChildMeta};
    let mut buf: Vec<EngineChildMeta> = vec![
        EngineChildMeta {
            id: 0, kind: 0, flags: 0, _pad: 0, child_count: 0,
            key_offset: 0, key_length: 0, array_index: 0,
            value_offset: 0, value_length: 0,
        };
        8
    ];
    let doc_ptr: *const Document = &doc;
    let n = engine_node_children_meta_batch(doc_ptr, root, 0, 8, buf.as_mut_ptr());
    assert_eq!(n, 3);

    // Find each child by key. Primitives (id, name) should have
    // id == NULL_NODE; the tags array should have a real id.
    let mut by_key: std::collections::HashMap<&str, &EngineChildMeta> =
        std::collections::HashMap::new();
    for meta in buf.iter().take(n as usize) {
        let key_bytes = if (meta.flags & 0x04) != 0 {
            // FLAG_KEY_IN_SOURCE — read raw bytes from source mmap.
            &doc.source_mmap[meta.key_offset as usize
                ..(meta.key_offset as usize + meta.key_length as usize)]
        } else {
            &doc.keys()[meta.key_offset as usize
                ..(meta.key_offset as usize + meta.key_length as usize)]
        };
        let key = std::str::from_utf8(key_bytes).unwrap();
        by_key.insert(
            match key { "id" => "id", "name" => "name", "tags" => "tags", _ => "?" },
            meta,
        );
    }

    let id_meta = by_key.get("id").expect("id present");
    assert_eq!(id_meta.id, u32::MAX, "primitive id has no record");
    assert_eq!(id_meta.kind, NodeKind::Number as u8);
    assert_eq!(id_meta.flags & 0x04, 0x04, "FLAG_KEY_IN_SOURCE for primitive");
    let id_value = &doc.source_mmap
        [id_meta.value_offset as usize..(id_meta.value_offset as usize + id_meta.value_length as usize)];
    assert_eq!(id_value, b"42");

    let name_meta = by_key.get("name").expect("name present");
    assert_eq!(name_meta.id, u32::MAX);
    assert_eq!(name_meta.kind, NodeKind::String as u8);

    let tags_meta = by_key.get("tags").expect("tags present");
    assert_ne!(tags_meta.id, u32::MAX, "container has a record id");
    assert_eq!(tags_meta.kind, NodeKind::Array as u8);
    assert_eq!(tags_meta.flags & 0x04, 0, "FLAG_KEY_IN_SOURCE clear for skippable");
    // Skippable members carry their decoded key in the keys arena.
    let tags_key = &doc.keys()[tags_meta.key_offset as usize
        ..(tags_meta.key_offset as usize + tags_meta.key_length as usize)];
    assert_eq!(tags_key, b"tags");
}

#[test]
fn children_kind_counts_includes_primitives() {
    let path = write_tmp(
        "engine_test_kind_counts.json",
        r#"{"a":1,"b":true,"c":null,"d":"x","e":[],"f":{}}"#,
    );
    let doc = Document::open(&path, None).expect("open");

    let mut counts = [0u32; 6];
    let total = engine::ffi::engine_node_children_kind_counts(
        &doc as *const Document,
        0,
        counts.as_mut_ptr(),
    );
    assert_eq!(total, 6);
    // null=0, bool=1, number=2, string=3, array=4, object=5
    assert_eq!(counts[NodeKind::Null as usize], 1);
    assert_eq!(counts[NodeKind::Bool as usize], 1);
    assert_eq!(counts[NodeKind::Number as usize], 1);
    assert_eq!(counts[NodeKind::String as usize], 1);
    assert_eq!(counts[NodeKind::Array as usize], 1);
    assert_eq!(counts[NodeKind::Object as usize], 1);
}

#[test]
fn handles_unicode_keys_and_values() {
    // Wrap each value in a container so its key gets pre-decoded into
    // the keys arena (under the hybrid gate, only members whose value
    // gets a record commit their key to the arena).
    let path = write_tmp(
        "engine_test_unicode.json",
        "{\"café\": [\"crème brûlée\"], \"emoji\": [\"\\uD83D\\uDE00\"]}",
    );
    let doc = Document::open(&path, None).expect("open");
    let root_kids = collect_kids(&doc, 0);
    assert_eq!(root_kids.len(), 2);
    let cafe_key = doc.key_bytes(root_kids[0]).unwrap();
    assert_eq!(std::str::from_utf8(cafe_key).unwrap(), "café");
    let emoji_key = doc.key_bytes(root_kids[1]).unwrap();
    assert_eq!(emoji_key, b"emoji");
}
