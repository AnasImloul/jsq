//! Foreign-key indexes. A `ForeignKeyIndex` is a precomputed map from key
//! values (extracted by a `key` AST) to the node IDs of the source items
//! that produced them. `lookup(SOURCE; KEY)` consults the registry by the
//! canonical string forms of its two AST arms; on hit it's an O(1)
//! hashmap fetch instead of a nested-loop scan.

use std::collections::HashMap;

use crate::document::{Document, NodeKind};
use crate::query::ast::Ast;
use crate::query::evaluator::walk_eval;
use crate::query::value::{decode_json_string, Scalar, Value};

/// Hashable normalization of a query value. Mirrors the `Scalar` shape
/// but uses an integer-or-float discriminant so we can derive `Hash`
/// (raw `f64` isn't `Eq`/`Hash`).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ScalarKey {
    Null,
    Bool(bool),
    /// Integral-valued numbers fold to a 64-bit integer so `1` and `1.0`
    /// share a bucket. Out-of-range or fractional values fall through to
    /// `Float` (bit-pattern keyed; NaN buckets together).
    Int(i64),
    Float(u64),
    Str(String),
}

/// Total order across the variants. Used to sort emission rows in a
/// canonical, allocation-free way (sequential render-to-String at
/// emission was the main cost on high-cardinality outputs). The
/// inter-variant order matches `ScalarKey`'s declaration:
/// Null < Bool < Int < Float < Str. Within `Float`, ties on the
/// stored bit pattern (NaN buckets together — same as `Hash`/`Eq`).
impl Ord for ScalarKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self, other) {
            (ScalarKey::Null, ScalarKey::Null) => Equal,
            (ScalarKey::Bool(a), ScalarKey::Bool(b)) => a.cmp(b),
            (ScalarKey::Int(a), ScalarKey::Int(b)) => a.cmp(b),
            (ScalarKey::Float(a), ScalarKey::Float(b)) => {
                let fa = f64::from_bits(*a);
                let fb = f64::from_bits(*b);
                fa.partial_cmp(&fb).unwrap_or_else(|| a.cmp(b))
            }
            (ScalarKey::Str(a), ScalarKey::Str(b)) => a.cmp(b),
            // Cross-variant: order by tag.
            _ => self.tag().cmp(&other.tag()),
        }
    }
}

impl PartialOrd for ScalarKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl ScalarKey {
    fn tag(&self) -> u8 {
        match self {
            ScalarKey::Null => 0,
            ScalarKey::Bool(_) => 1,
            ScalarKey::Int(_) => 2,
            ScalarKey::Float(_) => 3,
            ScalarKey::Str(_) => 4,
        }
    }

    /// Renders this key as a stable string for display (used as the
    /// row path when a `Value::Group` carries a `ScalarKey`-derived
    /// label) and as a sort key when the engine emits buckets in
    /// canonical order. Allocates only at emission, not per row.
    pub fn render_as_string(&self) -> String {
        match self {
            ScalarKey::Null => "null".to_string(),
            ScalarKey::Bool(true) => "true".to_string(),
            ScalarKey::Bool(false) => "false".to_string(),
            ScalarKey::Int(i) => format!("{}", i),
            ScalarKey::Float(bits) => {
                let n = f64::from_bits(*bits);
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e16 {
                    format!("{}", n as i64)
                } else {
                    format!("{}", n)
                }
            }
            ScalarKey::Str(s) => s.clone(),
        }
    }

    pub fn from_scalar(s: &Scalar) -> Option<Self> {
        match s {
            Scalar::Null => Some(ScalarKey::Null),
            Scalar::Bool(b) => Some(ScalarKey::Bool(*b)),
            Scalar::Number(n) => Some(number_key(*n)),
            Scalar::Str(s) => Some(ScalarKey::Str(s.clone())),
            Scalar::Container(_) => None,
        }
    }

    /// Builds a key directly from a node's value bytes — saves the alloc
    /// `Scalar::from_value` would do for strings, and avoids touching the
    /// `Document` lookups for the common primitive cases.
    pub fn from_node(doc: &Document, id: u32) -> Option<Self> {
        match doc.node_kind(id) {
            NodeKind::Null => Some(ScalarKey::Null),
            NodeKind::Bool => Some(ScalarKey::Bool(matches!(
                doc.value_bytes(id),
                Some(b"true")
            ))),
            NodeKind::Number => {
                let bytes = doc.value_bytes(id)?;
                let s = std::str::from_utf8(bytes).ok()?;
                let n: f64 = s.parse().ok()?;
                Some(number_key(n))
            }
            NodeKind::String => {
                let bytes = doc.value_bytes(id)?;
                Some(ScalarKey::Str(decode_json_string(bytes)))
            }
            NodeKind::Array | NodeKind::Object => None,
        }
    }

    pub fn from_value(doc: &Document, v: &Value) -> Option<Self> {
        match v {
            Value::Node(id) => Self::from_node(doc, *id),
            other => Self::from_scalar(&Scalar::from_value(doc, other)),
        }
    }
}

fn number_key(n: f64) -> ScalarKey {
    if !n.is_finite() {
        return ScalarKey::Float(n.to_bits());
    }
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
        ScalarKey::Int(n as i64)
    } else {
        ScalarKey::Float(n.to_bits())
    }
}

/// One foreign-key index. Holds the bucketed map plus a small bit of
/// build-time metadata for surfacing in the UI ("indexed 12,431 items").
#[derive(Default, Debug)]
pub struct ForeignKeyIndex {
    pub buckets: HashMap<ScalarKey, Vec<u32>>,
    /// Number of source items that produced a usable key — i.e. the
    /// total count summed across buckets. Items whose key was missing or
    /// non-scalar are silently skipped (same shape as `lookup`'s scalar
    /// equality, which never matches a container).
    pub indexed_count: usize,
    /// Number of source items the build saw, including those that
    /// produced no usable key. `indexed_count <= source_count`.
    pub source_count: usize,
}

impl ForeignKeyIndex {
    pub fn build(doc: &Document, source: &Ast, key: &Ast) -> Self {
        // Two-phase build to avoid borrowing the registry mutably during
        // a walk — collect candidate node IDs first, then evaluate the
        // key expression for each one.
        let mut candidates: Vec<u32> = Vec::new();
        walk_eval(doc, source, Value::Node(0), &mut |v| {
            if let Value::Node(id) = v {
                candidates.push(id);
            }
            true
        });

        let mut buckets: HashMap<ScalarKey, Vec<u32>> = HashMap::new();
        let mut indexed = 0usize;
        let source_count = candidates.len();
        for id in candidates {
            let mut sk: Option<ScalarKey> = None;
            walk_eval(doc, key, Value::Node(id), &mut |k| {
                if sk.is_some() {
                    return false;
                }
                sk = ScalarKey::from_value(doc, &k);
                false
            });
            if let Some(sk) = sk {
                buckets.entry(sk).or_default().push(id);
                indexed += 1;
            }
        }

        ForeignKeyIndex {
            buckets,
            indexed_count: indexed,
            source_count,
        }
    }

    pub fn get(&self, key: &ScalarKey) -> Option<&[u32]> {
        self.buckets.get(key).map(|v| v.as_slice())
    }

    /// Approximate retained heap, for the "memory cost" UI hint. Counts
    /// string keys + per-bucket Vec overhead. Under-counts hashmap
    /// bookkeeping but is the right order of magnitude.
    pub fn approx_bytes(&self) -> usize {
        let mut total = 0usize;
        for (k, v) in &self.buckets {
            total += match k {
                ScalarKey::Str(s) => s.len() + 24,
                _ => 24,
            };
            total += v.len() * std::mem::size_of::<u32>() + 24;
        }
        total
    }
}

#[derive(Default, Debug)]
pub struct IndexRegistry {
    /// Keyed by (canonical SOURCE AST, canonical KEY AST). Both strings
    /// come from `Ast`'s `Display` impl, so structurally equal ASTs
    /// share an entry.
    pub entries: HashMap<(String, String), ForeignKeyIndex>,
}

impl IndexRegistry {
    pub fn insert(&mut self, source: String, key: String, index: ForeignKeyIndex) {
        self.entries.insert((source, key), index);
    }

    pub fn remove(&mut self, source: &str, key: &str) -> bool {
        self.entries
            .remove(&(source.to_string(), key.to_string()))
            .is_some()
    }

    pub fn get(&self, source: &str, key: &str) -> Option<&ForeignKeyIndex> {
        self.entries.get(&(source.to_string(), key.to_string()))
    }

    pub fn list(&self) -> Vec<(String, String, &ForeignKeyIndex)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .map(|((s, k), idx)| (s.clone(), k.clone(), idx))
            .collect();
        out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        out
    }
}
