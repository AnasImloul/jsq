//! Fingerprinting and group-key extraction.
//!
//! - [`first_scalar_key`] turns the first emission of an expression
//!   into a typed `ScalarKey` for bucketed grouping.
//! - [`value_to_group_key`] renders a value as a string for
//!   composite (KeyTuple) group keys.
//! - [`write_fingerprint`] produces a stable byte-level hash key for
//!   `distinct` dedup. Engine nodes fingerprint by their raw source
//!   bytes; synthetic values fingerprint by a kind-tagged JSON-like
//!   encoding so a synthetic string `"hello"` fingerprints identically
//!   to a node string whose bytes are `"hello"`.

use crate::document::Document;

use super::super::ast::Ast;
use super::super::index::ScalarKey;
use super::super::value::{Scalar, Value};
use super::render::format_number;
use super::walk::walk;

/// Evaluates `expr` against `input` and returns its first emitted value
/// normalised to a `ScalarKey`. Returns `None` when the expression emits
/// nothing or yields a non-scalar value.
pub(super) fn first_scalar_key(doc: &Document, expr: &Ast, input: &Value) -> Option<ScalarKey> {
    let mut found: Option<ScalarKey> = None;
    walk(doc, expr, input.clone(), &mut |v| {
        if found.is_none() {
            found = ScalarKey::from_value(doc, &v);
        }
        false
    });
    found
}

/// Renders a value as the string segment of a composite group key.
/// Used by `Ast::KeyTuple` to assemble multi-component keys joined
/// with U+001F.
pub(super) fn value_to_group_key(doc: &Document, v: &Value) -> String {
    match Scalar::from_value(doc, v) {
        Scalar::Null => "null".to_string(),
        Scalar::Bool(b) => if b { "true".into() } else { "false".into() },
        Scalar::Number(n) => format_number(n),
        Scalar::Str(s) => s,
        Scalar::Container(_) => "(container)".to_string(),
    }
}

/// Writes into a caller-supplied buffer so the hot loop can reuse one
/// allocation across an entire stream — see the `Ast::Distinct` arm in
/// the pipe handler.
pub(super) fn write_fingerprint(doc: &Document, v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Node(id) => {
            // Tag with a kind byte so a synthetic JSON-string `"5"`
            // never collides with a node *number* `5` whose source
            // bytes also spell `5`. Within a single tag, raw bytes are
            // sufficient.
            out.push(b'N');
            if let Some(bytes) = doc.value_bytes(*id) {
                out.extend_from_slice(bytes);
            } else {
                // Container — fall back to a stable placeholder keyed
                // on the node id. Two separate container nodes with
                // the same shape compare unequal here, mirroring how
                // the rest of the evaluator treats containers.
                out.extend_from_slice(b"#");
                out.extend_from_slice(id.to_le_bytes().as_ref());
            }
        }
        Value::Null => out.extend_from_slice(b"Snull"),
        Value::Bool(true) => out.extend_from_slice(b"Strue"),
        Value::Bool(false) => out.extend_from_slice(b"Sfalse"),
        Value::Number(n) => {
            out.push(b'#');
            out.extend_from_slice(format_number(*n).as_bytes());
        }
        Value::Str(s) => {
            out.push(b'"');
            out.extend_from_slice(s.as_bytes());
            out.push(b'"');
        }
        Value::Group { key, n } => {
            out.extend_from_slice(b"G(");
            out.extend_from_slice(key.as_bytes());
            out.push(b',');
            match n {
                Some(v) => out.extend_from_slice(format_number(*v).as_bytes()),
                None => out.extend_from_slice(b"-"),
            }
            out.push(b')');
        }
        Value::GroupList { key, members } => {
            out.extend_from_slice(b"GL(");
            out.extend_from_slice(key.as_bytes());
            out.push(b',');
            out.extend_from_slice(members.len().to_le_bytes().as_ref());
            out.push(b')');
        }
        Value::Object(fields) | Value::BucketRow(fields) => {
            out.push(b'O');
            out.push(b'{');
            for (i, (name, value)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.push(b'"');
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b"\":");
                write_fingerprint(doc, value, out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'A');
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_fingerprint(doc, item, out);
            }
            out.push(b']');
        }
        Value::NamedValue { name, value } => {
            out.push(b'V');
            out.push(b'{');
            out.push(b'"');
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b"\":");
            write_fingerprint(doc, value, out);
            out.push(b'}');
        }
    }
}
