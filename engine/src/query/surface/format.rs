//! Pretty-printer for the surface AST. Turns any `Query` value back
//! into source text using a fixed two-space indent and one
//! item-per-line layout for multi-element constructs.
//!
//! The output always re-parses to a structurally identical `Query`,
//! modulo whitespace.

use super::super::ast::CompareOp;
use super::super::grammar::kw;
use super::ast::*;

const INDENT: &str = "  ";
const INDENT2: &str = "    ";

/// Soft line-width target for greedy field-set packing.
const MAX_WIDTH: usize = 80;

pub fn format_query(q: &Query) -> String {
    let mut out = String::new();

    for lb in &q.lets {
        format_let_binding(&mut out, lb);
        out.push('\n');
    }
    if !q.lets.is_empty() {
        out.push('\n');
    }

    // `from PATH as ALIAS`
    out.push_str(kw::FROM);
    out.push(' ');
    format_path(&mut out, &q.source.path);
    out.push(' ');
    out.push_str(kw::AS);
    out.push(' ');
    out.push_str(&q.source.alias);
    out.push('\n');

    // `join PATH as ALIAS on EXPR` — each on its own line, with the
    // `on …` aligned under the join keyword for scannable predicates.
    for j in &q.joins {
        out.push_str(kw::JOIN);
        out.push(' ');
        format_path(&mut out, &j.path);
        out.push(' ');
        out.push_str(kw::AS);
        out.push(' ');
        out.push_str(&j.alias);
        out.push('\n');
        out.push_str(INDENT);
        out.push_str(kw::ON);
        out.push(' ');
        format_expr(&mut out, &j.on);
        out.push('\n');
    }

    if let Some(pred) = &q.where_clause {
        format_where(&mut out, pred);
    }

    if !q.alias_lets.is_empty() {
        out.push_str(kw::LET);
        out.push(' ');
        let cont_indent = " ".repeat(kw::LET.len() + 1);
        for (i, al) in q.alias_lets.iter().enumerate() {
            if i > 0 {
                out.push_str(&cont_indent);
            }
            out.push_str(&al.name);
            out.push_str(" = ");
            format_expr(&mut out, &al.expr);
            if i + 1 < q.alias_lets.len() {
                out.push(',');
            }
            out.push('\n');
        }
    }

    if q.distinct {
        out.push_str(kw::DISTINCT);
        out.push('\n');
    }

    if !q.partitions.is_empty() {
        format_partition_block(&mut out, &q.partitions);
    }

    if let Some(agg) = &q.aggregate {
        match agg {
            AggregateClause::Shorthand(s) => format_shorthand(&mut out, s),
            AggregateClause::Block(b) => format_aggregate_block(&mut out, b),
            AggregateClause::Group(g) => format_group(&mut out, g),
            AggregateClause::EachPartition(ep) => format_each_partition(&mut out, ep),
        }
    }

    if let Some(fields) = &q.project {
        format_select(&mut out, fields);
    }

    if !q.order_by.is_empty() {
        format_order_by(&mut out, &q.order_by);
    }

    if let Some(n) = q.limit {
        out.push_str(&format!("{} {}\n", kw::LIMIT, n));
    }

    while out.ends_with('\n') {
        out.pop();
    }
    out
}

// ---- top-level clauses ----

fn format_let_binding(out: &mut String, lb: &LetBinding) {
    out.push_str(kw::LET);
    out.push(' ');
    out.push_str(&lb.name);
    out.push_str(" = {\n");
    for (i, f) in lb.fields.iter().enumerate() {
        out.push_str(INDENT);
        out.push_str(f);
        if i + 1 < lb.fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push('}');
}

fn format_where(out: &mut String, pred: &Expr) {
    out.push_str(kw::WHERE);
    out.push(' ');
    let conjuncts = collect_and_chain(pred);
    let mut first = true;
    for c in conjuncts {
        if first {
            format_predicate_term(out, c, kw::WHERE.len() + 1);
            first = false;
        } else {
            out.push('\n');
            out.push_str(kw::AND);
            out.push(' ');
            format_predicate_term(out, c, kw::AND.len() + 1);
        }
    }
    out.push('\n');
}

fn format_predicate_term(out: &mut String, e: &Expr, column: usize) {
    if let Expr::FieldSetCompare { base, items, op, rhs } = e {
        let mut inline = String::new();
        format_expr(&mut inline, e);
        if column + inline.len() <= MAX_WIDTH {
            out.push_str(&inline);
            return;
        }
        format_path(out, base);
        if !out.ends_with('.') {
            out.push('.');
        }
        out.push_str("{\n");
        let item_strs: Vec<String> = items.iter().map(render_field_set_item).collect();
        pack_items(out, &item_strs, INDENT2, MAX_WIDTH);
        out.push_str(INDENT);
        out.push_str("} ");
        out.push_str(format_compare_op(*op));
        out.push(' ');
        format_expr(out, rhs);
        return;
    }
    format_expr(out, e);
}

fn render_field_set_item(item: &FieldSetItem) -> String {
    let mut s = String::new();
    match item {
        FieldSetItem::Field(name) => s.push_str(name),
        FieldSetItem::Spread(name) => {
            s.push_str("...");
            s.push_str(name);
        }
        FieldSetItem::Override(name, value) => {
            s.push_str(name);
            s.push_str(": ");
            format_expr(&mut s, value);
        }
    }
    s
}

fn pack_items(out: &mut String, items: &[String], indent: &str, max_width: usize) {
    if items.is_empty() {
        return;
    }
    out.push_str(indent);
    let mut line_len = indent.len();
    let mut first_on_line = true;
    for (i, item) in items.iter().enumerate() {
        let is_last = i + 1 == items.len();
        let suffix = if is_last { "," } else { ", " };
        let needed = item.len() + suffix.len();
        if !first_on_line && line_len + needed > max_width {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            out.push_str(indent);
            line_len = indent.len();
        }
        out.push_str(item);
        out.push_str(suffix);
        line_len += needed;
        first_on_line = false;
    }
    out.push('\n');
}

fn collect_and_chain<'a>(e: &'a Expr) -> Vec<&'a Expr> {
    let mut out = Vec::new();
    flatten_and(e, &mut out);
    out
}

fn flatten_and<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::And(l, r) = e {
        flatten_and(l, out);
        flatten_and(r, out);
    } else {
        out.push(e);
    }
}

fn format_agg_op(op: AggOp) -> &'static str {
    match op {
        AggOp::Sum => kw::SUM,
        AggOp::Count => kw::COUNT,
        AggOp::Avg => kw::AVG,
        AggOp::Min => kw::MIN,
        AggOp::Max => kw::MAX,
    }
}

fn format_shorthand(out: &mut String, s: &AggregateShorthand) {
    out.push_str(format_agg_op(s.op));
    if let Some(arg) = &s.arg {
        out.push(' ');
        format_expr(out, arg);
    }
    if !s.group_by.is_empty() {
        out.push(' ');
        out.push_str(kw::BY);
        out.push(' ');
        for (i, k) in s.group_by.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            format_expr(out, k);
        }
    }
    out.push('\n');
}

fn format_group(out: &mut String, g: &GroupClause) {
    out.push_str(kw::GROUP);
    out.push(' ');
    out.push_str(kw::BY);
    out.push(' ');
    format_expr(out, &g.key);
    out.push('\n');
}

fn format_partition_block(out: &mut String, partitions: &[PartitionDef]) {
    out.push_str(kw::PARTITION);
    out.push_str(" {\n");
    for (i, p) in partitions.iter().enumerate() {
        out.push_str(INDENT);
        out.push_str(&p.name);
        out.push_str(": ");
        format_expr(out, &p.pred);
        if i + 1 < partitions.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("}\n");
}

fn format_aggregate_block(out: &mut String, b: &AggregateBlock) {
    out.push_str(kw::AGGREGATE);
    out.push_str(" {\n");
    for (i, item) in b.reductions.iter().enumerate() {
        out.push_str(INDENT);
        out.push_str(&item.name);
        out.push_str(": ");
        match &item.output {
            Expr::Object(fields) => format_object_multiline(out, fields, /* depth */ 1),
            other => format_expr(out, other),
        }
        if let Some(pred) = &item.where_pred {
            out.push(' ');
            out.push_str(kw::WHERE);
            out.push(' ');
            format_expr(out, pred);
        }
        if let Some(default) = &item.default {
            out.push_str(" ?? ");
            format_expr(out, default);
        }
        if i + 1 < b.reductions.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push('}');
    if let Some(group) = &b.group_by {
        out.push(' ');
        out.push_str(kw::BY);
        out.push(' ');
        format_expr(out, group);
    }
    out.push('\n');
}

fn format_each_partition(out: &mut String, ep: &EachPartitionClause) {
    out.push_str(kw::AGGREGATE);
    out.push(' ');
    out.push_str(kw::EACH);
    out.push(' ');
    out.push_str(kw::PARTITION);
    out.push(' ');
    out.push_str(kw::AS);
    out.push(' ');
    out.push_str(&ep.partition_alias);
    out.push_str(" => ");
    out.push_str(&ep.partition_alias);
    out.push_str(".name: ");
    match &ep.body {
        Expr::Object(fields) => format_object_multiline(out, fields, /* depth */ 0),
        other => format_expr(out, other),
    }
    out.push('\n');
}

fn format_object_multiline(out: &mut String, fields: &[ObjectField], depth: usize) {
    let inner_indent = INDENT.repeat(depth + 1);
    let close_indent = INDENT.repeat(depth);
    out.push_str("{\n");
    for (i, f) in fields.iter().enumerate() {
        out.push_str(&inner_indent);
        out.push_str(&f.name);
        out.push_str(": ");
        match &f.value {
            Expr::Object(inner) => format_object_multiline(out, inner, depth + 1),
            other => format_expr(out, other),
        }
        if let Some(d) = &f.default {
            out.push_str(" ?? ");
            format_expr(out, d);
        }
        if i + 1 < fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(&close_indent);
    out.push('}');
}

fn format_select(out: &mut String, fields: &[(String, Expr)]) {
    out.push_str(kw::SELECT);
    out.push_str(" {\n");
    for (i, (name, expr)) in fields.iter().enumerate() {
        out.push_str(INDENT);
        out.push_str(name);
        out.push_str(": ");
        format_expr(out, expr);
        if i + 1 < fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("}\n");
}

fn format_order_by(out: &mut String, keys: &[OrderKey]) {
    out.push_str(kw::ORDER);
    out.push(' ');
    out.push_str(kw::BY);
    out.push(' ');
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        format_expr(out, &k.expr);
        if k.desc {
            out.push(' ');
            out.push_str(kw::DESC);
        }
    }
    out.push('\n');
}

// ---- expressions ----

fn format_expr(out: &mut String, e: &Expr) {
    match e {
        Expr::Path(p) => format_path(out, p),
        Expr::Lit(l) => format_lit(out, l),
        Expr::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                format_expr(out, it);
            }
            out.push(']');
        }
        Expr::FieldSetCompare { base, items, op, rhs } => {
            format_path(out, base);
            if !out.ends_with('.') {
                out.push('.');
            }
            out.push('{');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match item {
                    FieldSetItem::Field(name) => out.push_str(name),
                    FieldSetItem::Spread(name) => {
                        out.push_str("...");
                        out.push_str(name);
                    }
                    FieldSetItem::Override(name, value) => {
                        out.push_str(name);
                        out.push_str(": ");
                        format_expr(out, value);
                    }
                }
            }
            out.push_str("} ");
            out.push_str(&format_compare_op(*op));
            out.push(' ');
            format_expr(out, rhs);
        }
        Expr::Compare(l, op, r) => {
            format_expr(out, l);
            out.push(' ');
            out.push_str(&format_compare_op(*op));
            out.push(' ');
            format_expr(out, r);
        }
        Expr::In(l, r) => {
            format_expr(out, l);
            out.push(' ');
            out.push_str(kw::IN);
            out.push(' ');
            format_expr(out, r);
        }
        Expr::NotIn(l, r) => {
            format_expr(out, l);
            out.push(' ');
            out.push_str(kw::NOT);
            out.push(' ');
            out.push_str(kw::IN);
            out.push(' ');
            format_expr(out, r);
        }
        Expr::And(l, r) => {
            format_expr(out, l);
            out.push(' ');
            out.push_str(kw::AND);
            out.push(' ');
            format_expr(out, r);
        }
        Expr::Or(l, r) => {
            format_expr(out, l);
            out.push(' ');
            out.push_str(kw::OR);
            out.push(' ');
            format_expr(out, r);
        }
        Expr::Not(inner) => {
            out.push_str(kw::NOT);
            out.push(' ');
            format_expr(out, inner);
        }
        Expr::Exists(inner) => {
            format_expr(out, inner);
            out.push(' ');
            out.push_str(kw::EXISTS);
        }
        Expr::Binary { op, lhs, rhs } => format_binary(out, *op, lhs, rhs),
        Expr::Neg(inner) => {
            out.push('-');
            format_atom(out, inner);
        }
        Expr::Reducer { op, arg } => {
            out.push_str(format_agg_op(*op));
            out.push('(');
            if let Some(a) = arg {
                format_expr(out, a);
            }
            out.push(')');
        }
        Expr::Object(fields) => {
            out.push('{');
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&f.name);
                out.push_str(": ");
                format_expr(out, &f.value);
                if let Some(d) = &f.default {
                    out.push_str(" ?? ");
                    format_expr(out, d);
                }
            }
            out.push('}');
        }
        Expr::TypeTest { value, kind, negated } => {
            format_expr(out, value);
            out.push(' ');
            out.push_str(kw::IS);
            if *negated {
                out.push(' ');
                out.push_str(kw::NOT);
            }
            out.push(' ');
            out.push_str(kind.keyword());
        }
        Expr::Round { value, precision } => {
            out.push_str(kw::ROUND);
            out.push('(');
            format_expr(out, value);
            if let Some(p) = precision {
                out.push_str(", ");
                format_expr(out, p);
            }
            out.push(')');
        }
    }
}

fn format_binary(out: &mut String, op: BinaryOp, lhs: &Expr, rhs: &Expr) {
    format_binary_operand(out, op, lhs, true);
    out.push(' ');
    out.push_str(binary_op_str(op));
    out.push(' ');
    format_binary_operand(out, op, rhs, false);
}

fn format_binary_operand(out: &mut String, parent: BinaryOp, e: &Expr, is_left: bool) {
    let needs_parens = match e {
        Expr::Binary { op: child, .. } => {
            let p_parent = bin_prec(parent);
            let p_child = bin_prec(*child);
            p_child < p_parent
                || (p_child == p_parent
                    && !is_left
                    && matches!(parent, BinaryOp::Sub | BinaryOp::Div))
        }
        _ => false,
    };
    if needs_parens {
        out.push('(');
        format_expr(out, e);
        out.push(')');
    } else {
        format_expr(out, e);
    }
}

fn format_atom(out: &mut String, e: &Expr) {
    match e {
        Expr::Lit(_) | Expr::Path(_) | Expr::Reducer { .. } => format_expr(out, e),
        _ => {
            out.push('(');
            format_expr(out, e);
            out.push(')');
        }
    }
}

fn bin_prec(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Add | BinaryOp::Sub => 1,
        BinaryOp::Mul | BinaryOp::Div => 2,
    }
}

fn binary_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
    }
}

fn format_compare_op(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Le => "<=",
        CompareOp::Gt => ">",
        CompareOp::Ge => ">=",
        CompareOp::Contains => kw::CONTAINS,
        CompareOp::StartsWith => kw::STARTS_WITH,
        CompareOp::EndsWith => kw::ENDS_WITH,
        CompareOp::Matches => kw::MATCHES,
    }
}

fn format_lit(out: &mut String, l: &Lit) {
    match l {
        Lit::Number(n) => {
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e16 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{}", n));
            }
        }
        Lit::Str(s) => {
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Lit::Bool(true) => out.push_str(kw::TRUE),
        Lit::Bool(false) => out.push_str(kw::FALSE),
        Lit::Null => out.push_str(kw::NULL),
    }
}

fn format_path(out: &mut String, p: &PathExpr) {
    match &p.root {
        PathRoot::Identity => out.push('.'),
        PathRoot::Name(name) => out.push_str(name),
    }
    let mut first = true;
    for seg in &p.segments {
        match seg {
            PathSeg::Field(name) => {
                if first && matches!(p.root, PathRoot::Identity) {
                    out.push_str(name);
                } else {
                    out.push('.');
                    out.push_str(name);
                }
                first = false;
            }
            PathSeg::Index(i) => {
                first = false;
                out.push_str(&format!("[{}]", i));
            }
            PathSeg::Iterate => {
                first = false;
                out.push_str("[*]");
            }
            PathSeg::StarStar => {
                out.push_str(if first && matches!(p.root, PathRoot::Identity) {
                    "**"
                } else {
                    ".**"
                });
                first = false;
            }
            PathSeg::FieldSet(items) => {
                if first && matches!(p.root, PathRoot::Identity) {
                    out.push('{');
                } else {
                    out.push_str(".{");
                }
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match item {
                        FieldSetItem::Field(name) => out.push_str(name),
                        FieldSetItem::Spread(name) => {
                            out.push_str("...");
                            out.push_str(name);
                        }
                        FieldSetItem::Override(name, value) => {
                            out.push_str(name);
                            out.push_str(": ");
                            format_expr(out, value);
                        }
                    }
                }
                out.push('}');
                first = false;
            }
        }
    }
}
