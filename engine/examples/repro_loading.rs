// Reproduce the user's "Loading more..." stub bug. Open a JSON whose
// top-level object has 4 keys with long values, walk children via the
// resumable iterator, and verify we emit exactly 4 metas.

use engine::document::Document;
use engine::ffi::{
    engine_node_children_meta_batch_resume, EngineChildMeta, EngineScanState,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: repro_loading <path.json>");
    let doc = Document::open(std::path::Path::new(&path), None).expect("open");

    let root = 0u32;
    let r = doc.record(root).unwrap();
    println!("root.child_count   = {}", r.child_count);
    println!("root.subtree_size  = {}", r.subtree_size);
    println!("total records      = {}", doc.records().len());

    // Resumable walk.
    let mut state = EngineScanState {
        pos: u64::MAX, next_skippable: u32::MAX, array_index: 0,
    };
    let mut buf = vec![EngineChildMeta {
        id: 0, kind: 0, flags: 0, _pad: 0, child_count: 0,
        key_offset: 0, key_length: 0, array_index: 0,
        value_offset: 0, value_length: 0,
    }; 4]; // <-- match Swift's limit=4 case
    let doc_ptr: *const Document = &doc;
    let mut total = 0u32;
    let mut iter = 0;
    loop {
        iter += 1;
        let n = engine_node_children_meta_batch_resume(
            doc_ptr, root, &mut state, buf.len() as u32, buf.as_mut_ptr(),
        );
        println!("call {}: returned {}, state.pos = {}, state.next_skippable = {}",
            iter, n, state.pos, state.next_skippable);
        if n == 0 { break; }
        for i in 0..n as usize {
            let m = buf[i];
            println!("  child {}: id={} kind={} key_off={} key_len={} value_off={} value_len={}",
                total + i as u32, m.id, m.kind, m.key_offset, m.key_length,
                m.value_offset, m.value_length);
        }
        total += n;
        if iter > 5 { break; }
    }
    println!("total emitted: {} (expected: {})", total, r.child_count);
}
