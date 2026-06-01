//! Recursive-descent parser for the surface query language. Reuses the
//! engine's tokenizer (`super::super::lexer`).
//!
//! Clause order (pipeline-shaped, SQL keywords):
//!
//!   (fields NAME = { ... })*
//!   from PATH as ALIAS
//!   (join PATH as ALIAS on EXPR)*
//!   (where EXPR)?
//!   (let NAME = EXPR (, NAME = EXPR)*)?
//!   distinct?
//!   (aggregate-clause)?      // `aggregate { ... }` block | `collect by`
//!   (select { NAME: EXPR, ... })?
//!   (order by EXPR [asc|desc] (, EXPR [asc|desc])*)?
//!   (limit N)?

use super::super::ast::CompareOp;
use super::super::grammar::kw;
use super::super::lexer::{tokenize, Token, TokenKind};
use super::super::QueryError;
use super::ast::{
    AggBlockItem, AggOp, AggregateBlock, AggregateClause, AliasLet, BinaryOp, Expr, FieldSetDef,
    FieldSetItem, GroupClause, JoinClause, JoinKind, JsonTypeKind, Lit, ObjectField, OrderKey, PathExpr,
    PathRoot, PathSeg, Query, SourceClause, UnnestClause,
};

fn op_keyword(op: AggOp) -> &'static str {
    match op {
        AggOp::Sum => kw::SUM,
        AggOp::Count => kw::COUNT,
        AggOp::Avg => kw::AVG,
        AggOp::Min => kw::MIN,
        AggOp::Max => kw::MAX,
    }
}

pub fn parse(source: &str) -> Result<Query, QueryError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, pos: 0 };
    let query = parser.parse_query()?;
    if !matches!(parser.peek().kind, TokenKind::Eof) {
        return Err(QueryError::new(
            parser.peek().position,
            format!("unexpected {} after end of query", parser.peek().kind.description()),
        ));
    }
    Ok(query)
}

/// Parses just a path expression — used by index-building helpers that
/// need to canonicalize a path string without wrapping it in a full
/// `from … as … select …` boilerplate query.
pub fn parse_path_only(source: &str) -> Result<PathExpr, QueryError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, pos: 0 };
    let path = parser.parse_path(/* allow_field_set */ false)?;
    if !matches!(parser.peek().kind, TokenKind::Eof) {
        return Err(QueryError::new(
            parser.peek().position,
            format!(
                "unexpected {} after end of path",
                parser.peek().kind.description()
            ),
        ));
    }
    Ok(path)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn expect(&mut self, kind: &TokenKind, msg: &str) -> Result<(), QueryError> {
        if std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind) {
            self.pos += 1;
            Ok(())
        } else {
            Err(QueryError::new(self.peek().position, msg.to_string()))
        }
    }

    fn consume_keyword(&mut self, kw: &str) -> bool {
        if let TokenKind::Ident(s) = &self.peek().kind {
            if s == kw {
                self.advance();
                return true;
            }
        }
        false
    }

    fn peek_keyword(&self, kw: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(s) if s == kw)
    }

    /// Lookahead clamped to the trailing EOF token, so callers can peek
    /// past the cursor without bounds-checking.
    fn peek_at(&self, offset: usize) -> &Token {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[i]
    }

    fn parse_query(&mut self) -> Result<Query, QueryError> {
        // Top-of-query `fields NAME = { ... }` macros (compile-time only).
        let mut field_sets = Vec::new();
        while self.peek_keyword(kw::FIELDS) {
            self.advance();
            field_sets.push(self.parse_field_set_def()?);
        }

        // `from PATH as ALIAS` — mandatory.
        if !self.consume_keyword(kw::FROM) {
            return Err(QueryError::new(
                self.peek().position,
                "every query must start with `from PATH as ALIAS`".into(),
            ));
        }
        let source = self.parse_source_clause()?;

        // Zero or more joins, each optionally qualified `inner`/`left`.
        let mut joins = Vec::new();
        loop {
            let kind = if self.consume_keyword(kw::INNER) {
                if !self.consume_keyword(kw::JOIN) {
                    return Err(QueryError::new(
                        self.peek().position,
                        "expected `join` after `inner`".into(),
                    ));
                }
                JoinKind::Inner
            } else if self.consume_keyword(kw::LEFT) {
                if !self.consume_keyword(kw::JOIN) {
                    return Err(QueryError::new(
                        self.peek().position,
                        "expected `join` after `left`".into(),
                    ));
                }
                JoinKind::Left
            } else if self.consume_keyword(kw::JOIN) {
                JoinKind::Inner
            } else {
                break;
            };
            joins.push(self.parse_join_clause(kind)?);
        }

        // Zero or more `unnest EXPR as ALIAS` clauses, applied after the
        // joins and before `where`.
        let mut unnests = Vec::new();
        while self.consume_keyword(kw::UNNEST) {
            unnests.push(self.parse_unnest_clause()?);
        }

        let where_clause = if self.consume_keyword(kw::WHERE) {
            Some(self.parse_or()?)
        } else {
            None
        };

        // Post-where `let NAME = EXPR` aliases.
        let mut alias_lets = Vec::new();
        if self.peek_keyword(kw::LET) {
            self.advance();
            alias_lets.push(self.parse_alias_let()?);
            while matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                alias_lets.push(self.parse_alias_let()?);
            }
        }

        let distinct = self.consume_keyword(kw::DISTINCT);

        let aggregate = self.parse_aggregate_clause_opt()?;

        let having = if self.consume_keyword(kw::HAVING) {
            Some(self.parse_or()?)
        } else {
            None
        };

        let project = if self.consume_keyword(kw::SELECT) {
            Some(self.parse_select_block()?)
        } else {
            None
        };

        let order_by = if self.consume_keyword(kw::ORDER) {
            if !self.consume_keyword(kw::BY) {
                return Err(QueryError::new(
                    self.peek().position,
                    "expected `by` after `order`".into(),
                ));
            }
            self.parse_order_keys()?
        } else {
            Vec::new()
        };

        let limit = if self.consume_keyword(kw::LIMIT) {
            Some(self.parse_limit_value()?)
        } else {
            None
        };

        Ok(Query {
            field_sets,
            source,
            joins,
            unnests,
            where_clause,
            distinct,
            alias_lets,
            aggregate,
            having,
            project,
            order_by,
            limit,
        })
    }

    // ---- source / join ----

    fn parse_source_clause(&mut self) -> Result<SourceClause, QueryError> {
        let path = self.parse_path(/* allow_field_set */ false)?;
        if !self.consume_keyword(kw::AS) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `as ALIAS` after `from PATH`".into(),
            ));
        }
        let alias = self.parse_alias_ident()?;
        Ok(SourceClause { path, alias })
    }

    fn parse_join_clause(&mut self, kind: JoinKind) -> Result<JoinClause, QueryError> {
        let path = self.parse_path(/* allow_field_set */ false)?;
        if !self.consume_keyword(kw::AS) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `as ALIAS` after `join PATH`".into(),
            ));
        }
        let alias = self.parse_alias_ident()?;
        if !self.consume_keyword(kw::ON) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `on LEFT == RIGHT` after `join PATH as ALIAS`".into(),
            ));
        }
        let on = self.parse_or()?;
        Ok(JoinClause { path, alias, on, kind })
    }

    fn parse_unnest_clause(&mut self) -> Result<UnnestClause, QueryError> {
        let expr = self.parse_or()?;
        if !self.consume_keyword(kw::AS) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `as ALIAS` after `unnest EXPR`".into(),
            ));
        }
        let alias = self.parse_alias_ident()?;
        Ok(UnnestClause { expr, alias })
    }

    fn parse_alias_ident(&mut self) -> Result<String, QueryError> {
        let pos = self.peek().position;
        let name = match &self.peek().kind {
            TokenKind::Ident(s) => s.clone(),
            _ => {
                return Err(QueryError::new(
                    pos,
                    "expected an alias identifier after `as`".into(),
                ));
            }
        };
        // Reserved words can't be aliases — they'd shadow keywords in
        // alias-qualified paths (`from .x as where` is nonsense).
        if super::super::grammar::is_keyword(&name) {
            return Err(QueryError::new(
                pos,
                format!("alias `{}` is a reserved keyword", name),
            ));
        }
        self.advance();
        Ok(name)
    }

    // ---- alias let / partition ----

    fn parse_alias_let(&mut self) -> Result<AliasLet, QueryError> {
        let pos = self.peek().position;
        let name = match &self.peek().kind {
            TokenKind::Ident(s) => s.clone(),
            _ => {
                return Err(QueryError::new(
                    pos,
                    "expected identifier after `let`".into(),
                ));
            }
        };
        self.advance();
        self.expect(&TokenKind::Assign, "expected `=` after `let NAME`")?;
        let expr = self.parse_or()?;
        Ok(AliasLet { name, expr })
    }

    // ---- order by / limit ----

    fn parse_order_keys(&mut self) -> Result<Vec<OrderKey>, QueryError> {
        let mut keys = vec![self.parse_one_order_key()?];
        while matches!(self.peek().kind, TokenKind::Comma) {
            self.advance();
            keys.push(self.parse_one_order_key()?);
        }
        Ok(keys)
    }

    fn parse_one_order_key(&mut self) -> Result<OrderKey, QueryError> {
        let expr = self.parse_or()?;
        let desc = if self.consume_keyword(kw::DESC) {
            true
        } else if self.consume_keyword(kw::ASC) {
            false
        } else {
            false
        };
        Ok(OrderKey { expr, desc })
    }

    fn parse_limit_value(&mut self) -> Result<u64, QueryError> {
        match self.peek().kind {
            TokenKind::Number(n) => {
                if n.fract() != 0.0 || n < 0.0 {
                    return Err(QueryError::new(
                        self.peek().position,
                        format!("limit must be a non-negative integer, got {}", n),
                    ));
                }
                self.advance();
                Ok(n as u64)
            }
            _ => Err(QueryError::new(
                self.peek().position,
                "expected a non-negative integer after `limit`".into(),
            )),
        }
    }

    // ---- select projection ----

    fn parse_select_block(&mut self) -> Result<Vec<(String, Expr)>, QueryError> {
        self.expect(&TokenKind::LBrace, "expected `{` after `select`")?;
        let mut fields = Vec::new();
        loop {
            if matches!(self.peek().kind, TokenKind::RBrace) {
                break;
            }
            let pos = self.peek().position;
            let name = match &self.peek().kind {
                TokenKind::Ident(s) => s.clone(),
                TokenKind::Str(s) => s.clone(),
                _ => {
                    return Err(QueryError::new(
                        pos,
                        "expected field name in `select { ... }`".into(),
                    ));
                }
            };
            self.advance();
            self.expect(&TokenKind::Colon, "expected `:` after field name")?;
            let value = self.parse_or()?;
            fields.push((name, value));
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RBrace, "expected `}` to close select block")?;
        if fields.is_empty() {
            return Err(QueryError::new(
                self.peek().position,
                "`select { ... }` must contain at least one field".into(),
            ));
        }
        Ok(fields)
    }

    // ---- field-set macros (top-of-query `fields NAME = { ... }`) ----

    fn parse_field_set_def(&mut self) -> Result<FieldSetDef, QueryError> {
        let pos = self.peek().position;
        let name = match &self.peek().kind {
            TokenKind::Ident(s) => s.clone(),
            _ => {
                return Err(QueryError::new(
                    pos,
                    "expected identifier after `fields`".into(),
                ));
            }
        };
        self.advance();
        self.expect(&TokenKind::Assign, "expected `=` after `fields NAME`")?;
        self.expect(&TokenKind::LBrace, "expected `{` to start a field set")?;
        let mut fields = Vec::new();
        loop {
            if matches!(self.peek().kind, TokenKind::RBrace) {
                break;
            }
            let pos = self.peek().position;
            match &self.peek().kind {
                TokenKind::Ident(s) => {
                    fields.push(s.clone());
                    self.advance();
                }
                TokenKind::Str(s) => {
                    fields.push(s.clone());
                    self.advance();
                }
                _ => {
                    return Err(QueryError::new(
                        pos,
                        "expected field name in `fields` set".into(),
                    ));
                }
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RBrace, "expected `}` to end `fields` set")?;
        if fields.is_empty() {
            return Err(QueryError::new(
                self.peek().position,
                "`fields NAME = { ... }` must contain at least one field".into(),
            ));
        }
        Ok(FieldSetDef { name, fields })
    }

    // ---- aggregate ----

    fn parse_aggregate_clause_opt(&mut self) -> Result<Option<AggregateClause>, QueryError> {
        if self.peek_keyword(kw::AGGREGATE) {
            self.advance();
            return Ok(Some(AggregateClause::Block(self.parse_aggregate_block()?)));
        }
        if self.peek_keyword(kw::COLLECT) {
            self.advance();
            if !self.consume_keyword(kw::BY) {
                return Err(QueryError::new(
                    self.peek().position,
                    "expected `by` after `collect`".into(),
                ));
            }
            let key = self.parse_or()?;
            return Ok(Some(AggregateClause::Group(GroupClause { key })));
        }
        // A bare reducer keyword at clause position is not a valid clause —
        // reducer calls only exist inside an `aggregate { ... }` block.
        if self.peek_keyword(kw::SUM)
            || self.peek_keyword(kw::COUNT)
            || self.peek_keyword(kw::AVG)
            || self.peek_keyword(kw::MIN)
            || self.peek_keyword(kw::MAX)
        {
            let op_name = match &self.peek().kind {
                TokenKind::Ident(s) => s.clone(),
                _ => unreachable!(),
            };
            return Err(QueryError::new(
                self.peek().position,
                format!(
                    "reducer `{}` is only valid inside `aggregate {{ ... }}` — write \
                     `aggregate {{ <name>: {}(<expr>) }} [by KEY]` instead",
                    op_name, op_name
                ),
            ));
        }
        Ok(None)
    }

    fn parse_aggregate_block(&mut self) -> Result<AggregateBlock, QueryError> {
        self.expect(&TokenKind::LBrace, "expected `{` after `aggregate`")?;
        let mut reductions: Vec<AggBlockItem> = Vec::new();
        loop {
            if matches!(self.peek().kind, TokenKind::RBrace) {
                break;
            }
            reductions.push(self.parse_agg_block_item()?);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RBrace, "expected `}` to close aggregate block")?;
        if reductions.is_empty() {
            return Err(QueryError::new(
                self.peek().position,
                "aggregate block must contain at least one reduction".into(),
            ));
        }
        let (group_by, rollup) = if self.consume_keyword(kw::BY) {
            if self.consume_keyword(kw::ROLLUP) {
                self.expect(&TokenKind::LParen, "expected `(` after `rollup`")?;
                let keys = self.parse_group_keys()?;
                self.expect(
                    &TokenKind::RParen,
                    "expected `)` to close `rollup(...)`",
                )?;
                (keys, true)
            } else {
                (self.parse_group_keys()?, false)
            }
        } else {
            (Vec::new(), false)
        };
        Ok(AggregateBlock {
            reductions,
            group_by,
            rollup,
        })
    }

    fn parse_agg_block_item(&mut self) -> Result<AggBlockItem, QueryError> {
        let name = self.parse_simple_name()?;
        self.expect(
            &TokenKind::Colon,
            "expected `:` between reduction name and output expression",
        )?;
        let output = self.parse_or()?;
        let where_pred = if self.consume_keyword(kw::WHERE) {
            Some(self.parse_or()?)
        } else {
            None
        };
        let default = if matches!(self.peek().kind, TokenKind::QuestionQuestion) {
            self.advance();
            Some(self.parse_or()?)
        } else {
            None
        };
        Ok(AggBlockItem {
            name,
            output,
            where_pred,
            default,
        })
    }

    fn parse_object_field(&mut self) -> Result<ObjectField, QueryError> {
        let name = self.parse_simple_name()?;
        self.expect(
            &TokenKind::Colon,
            "expected `:` between object-field name and value",
        )?;
        let value = self.parse_or()?;
        let default = if matches!(self.peek().kind, TokenKind::QuestionQuestion) {
            self.advance();
            Some(self.parse_or()?)
        } else {
            None
        };
        Ok(ObjectField { name, value, default })
    }

    /// Plain identifier or quoted-string name (no brace expansion).
    fn parse_simple_name(&mut self) -> Result<String, QueryError> {
        let pos = self.peek().position;
        match &self.peek().kind {
            TokenKind::Ident(s) => {
                let n = s.clone();
                self.advance();
                Ok(n)
            }
            TokenKind::Str(s) => {
                let n = s.clone();
                self.advance();
                Ok(n)
            }
            _ => Err(QueryError::new(pos, "expected a name identifier".into())),
        }
    }

    fn parse_group_keys(&mut self) -> Result<Vec<Expr>, QueryError> {
        let mut keys = vec![self.parse_or()?];
        while matches!(self.peek().kind, TokenKind::Comma) {
            self.advance();
            keys.push(self.parse_or()?);
        }
        Ok(keys)
    }

    // ---- expressions ----

    fn parse_or(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.parse_and()?;
        while self.consume_keyword(kw::OR) {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.parse_not()?;
        while self.consume_keyword(kw::AND) {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, QueryError> {
        if self.consume_keyword(kw::NOT) {
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> Result<Expr, QueryError> {
        let left = self.parse_additive()?;

        if self.peek_keyword(kw::EXISTS) {
            self.advance();
            return Ok(Expr::Exists(Box::new(left)));
        }

        if self.peek_keyword(kw::IS) {
            self.advance();
            let negated = if self.peek_keyword(kw::NOT) {
                self.advance();
                true
            } else {
                false
            };
            let kind_pos = self.peek().position;
            let kind_ident = match &self.peek().kind {
                TokenKind::Ident(s) => s.clone(),
                _ => return Err(QueryError::new(
                    kind_pos,
                    "expected a JSON type name after `is`".into(),
                )),
            };
            let Some(kind) = JsonTypeKind::from_keyword(&kind_ident) else {
                return Err(QueryError::new(
                    kind_pos,
                    format!("unknown JSON type `{}` after `is`", kind_ident),
                ));
            };
            self.advance();
            return Ok(Expr::TypeTest {
                value: Box::new(left),
                kind,
                negated,
            });
        }

        if self.peek_keyword(kw::IN) {
            self.advance();
            let right = self.parse_additive()?;
            return Ok(Expr::In(Box::new(left), Box::new(right)));
        }
        if self.peek_keyword(kw::NOT) {
            if let Some(next) = self.tokens.get(self.pos + 1) {
                if matches!(&next.kind, TokenKind::Ident(s) if s == kw::IN) {
                    self.advance();
                    self.advance();
                    let right = self.parse_additive()?;
                    return Ok(Expr::NotIn(Box::new(left), Box::new(right)));
                }
            }
        }

        let op = match self.peek().kind {
            TokenKind::Eq => CompareOp::Eq,
            TokenKind::Ne => CompareOp::Ne,
            TokenKind::Lt => CompareOp::Lt,
            TokenKind::Le => CompareOp::Le,
            TokenKind::Gt => CompareOp::Gt,
            TokenKind::Ge => CompareOp::Ge,
            TokenKind::Ident(ref s) if matches!(
                s.as_str(),
                kw::MATCHES | kw::STARTS_WITH | kw::ENDS_WITH | kw::CONTAINS
            ) => match s.as_str() {
                kw::MATCHES => CompareOp::Matches,
                kw::STARTS_WITH => CompareOp::StartsWith,
                kw::ENDS_WITH => CompareOp::EndsWith,
                kw::CONTAINS => CompareOp::Contains,
                _ => unreachable!(),
            },
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_additive()?;

        if let Expr::Path(p) = &left {
            if let Some(PathSeg::FieldSet(items)) = p.segments.last() {
                let mut base = p.clone();
                let items = items.clone();
                base.segments.pop();
                return Ok(Expr::FieldSetCompare {
                    base,
                    items,
                    op,
                    rhs: Box::new(right),
                });
            }
        }

        Ok(Expr::Compare(Box::new(left), op, Box::new(right)))
    }

    fn parse_additive(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, QueryError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, QueryError> {
        if matches!(self.peek().kind, TokenKind::Minus) {
            self.advance();
            let inner = self.parse_unary()?;
            return Ok(Expr::Neg(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, QueryError> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::LParen => {
                self.advance();
                // A parenthesised group that opens with `from` (or a
                // leading `fields` macro) is a subquery, not a grouped
                // expression — parse a full nested query.
                if self.peek_keyword(kw::FROM) || self.peek_keyword(kw::FIELDS) {
                    let q = self.parse_query()?;
                    self.expect(&TokenKind::RParen, "expected `)` to close subquery")?;
                    Ok(Expr::Subquery(Box::new(q)))
                } else {
                    let inner = self.parse_or()?;
                    self.expect(&TokenKind::RParen, "expected `)`")?;
                    Ok(inner)
                }
            }
            TokenKind::LBrace => {
                self.advance();
                let mut fields: Vec<ObjectField> = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RBrace) {
                    fields.push(self.parse_object_field()?);
                    while matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                        if matches!(self.peek().kind, TokenKind::RBrace) {
                            break;
                        }
                        fields.push(self.parse_object_field()?);
                    }
                }
                self.expect(&TokenKind::RBrace, "expected `}` to close object literal")?;
                Ok(Expr::Object(fields))
            }
            TokenKind::LBrack => {
                self.advance();
                let mut items = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RBrack) {
                    items.push(self.parse_or()?);
                    while matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                        if matches!(self.peek().kind, TokenKind::RBrack) {
                            break;
                        }
                        items.push(self.parse_or()?);
                    }
                }
                self.expect(&TokenKind::RBrack, "expected `]`")?;
                Ok(Expr::Array(items))
            }
            TokenKind::Number(n) => {
                self.advance();
                Ok(Expr::Lit(Lit::Number(n)))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(Expr::Lit(Lit::Str(s)))
            }
            TokenKind::Param(name) => {
                self.advance();
                Ok(Expr::Param(name))
            }
            TokenKind::Dot => {
                let path = self.parse_path(/* allow_field_set */ true)?;
                Ok(Expr::Path(path))
            }
            TokenKind::Ident(name) => match name.as_str() {
                kw::TRUE => {
                    self.advance();
                    Ok(Expr::Lit(Lit::Bool(true)))
                }
                kw::FALSE => {
                    self.advance();
                    Ok(Expr::Lit(Lit::Bool(false)))
                }
                kw::NULL => {
                    self.advance();
                    Ok(Expr::Lit(Lit::Null))
                }
                kw::IF => self.parse_if(),
                kw::SUM | kw::COUNT | kw::AVG | kw::MIN | kw::MAX => self.parse_reducer_call(),
                // Strict scalar functions (`round`, `length`, `lower`, …)
                // when immediately applied. A bare function name not
                // followed by `(` falls through to the path parser so it
                // can still name a field.
                _ if super::super::grammar::function(name.as_str()).is_some()
                    && matches!(self.peek_at(1).kind, TokenKind::LParen) =>
                {
                    self.parse_call()
                }
                _ => {
                    // Bare-name path: `s`, `s.book_id`, etc.
                    let path = self.parse_path(/* allow_field_set */ true)?;
                    Ok(Expr::Path(path))
                }
            },
            _ => Err(QueryError::new(
                token.position,
                format!("unexpected {} (expected expression)", token.kind.description()),
            )),
        }
    }

    fn parse_reducer_call(&mut self) -> Result<Expr, QueryError> {
        let op_pos = self.peek().position;
        let op = match &self.peek().kind {
            TokenKind::Ident(s) if s == kw::SUM => AggOp::Sum,
            TokenKind::Ident(s) if s == kw::COUNT => AggOp::Count,
            TokenKind::Ident(s) if s == kw::AVG => AggOp::Avg,
            TokenKind::Ident(s) if s == kw::MIN => AggOp::Min,
            TokenKind::Ident(s) if s == kw::MAX => AggOp::Max,
            _ => unreachable!("parse_reducer_call called on non-reducer token"),
        };
        self.advance();
        if !matches!(self.peek().kind, TokenKind::LParen) {
            return Err(QueryError::new(
                self.peek().position,
                format!("expected `(` after `{}`", op_keyword(op)),
            ));
        }
        self.advance();
        let arg = if matches!(self.peek().kind, TokenKind::RParen) {
            if !matches!(op, AggOp::Count) {
                return Err(QueryError::new(
                    op_pos,
                    format!("`{}` requires an argument", op_keyword(op)),
                ));
            }
            None
        } else {
            Some(Box::new(self.parse_or()?))
        };
        self.expect(&TokenKind::RParen, "expected `)` to close reducer call")?;
        Ok(Expr::Reducer { op, arg })
    }

    /// Generic strict-function call `name(arg, ...)`. The leading ident is
    /// a known `grammar::FUNCTIONS` entry (the caller already checked) and
    /// is followed by `(`. Validates argument count against the spec.
    fn parse_call(&mut self) -> Result<Expr, QueryError> {
        let name_pos = self.peek().position;
        let name = match &self.peek().kind {
            TokenKind::Ident(s) => s.clone(),
            _ => unreachable!("parse_call invoked on non-ident"),
        };
        self.advance();
        self.expect(&TokenKind::LParen, "expected `(` after function name")?;
        let mut args = Vec::new();
        if !matches!(self.peek().kind, TokenKind::RParen) {
            args.push(self.parse_or()?);
            while matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                args.push(self.parse_or()?);
            }
        }
        self.expect(&TokenKind::RParen, "expected `)` to close function call")?;
        let spec = super::super::grammar::function(&name)
            .expect("parse_call invoked on unknown function");
        if args.len() < spec.min_args || args.len() > spec.max_args {
            let arity = if spec.min_args == spec.max_args {
                format!("{}", spec.min_args)
            } else {
                format!("{} to {}", spec.min_args, spec.max_args)
            };
            return Err(QueryError::new(
                name_pos,
                format!(
                    "`{}` takes {} argument(s), got {}",
                    name,
                    arity,
                    args.len()
                ),
            ));
        }
        Ok(Expr::Call(name, args))
    }

    /// `if(COND, THEN, ELSE)`. Exactly three comma-separated arguments;
    /// no two-arg form (use `?? null` explicitly if you want a missing
    /// `else`).
    fn parse_if(&mut self) -> Result<Expr, QueryError> {
        let kw_pos = self.peek().position;
        self.advance();
        self.expect(&TokenKind::LParen, "expected `(` after `if`")?;
        let cond = self.parse_or()?;
        if !matches!(self.peek().kind, TokenKind::Comma) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `,` after `if` condition — `if` requires three arguments \
                 `if(COND, THEN, ELSE)`"
                    .into(),
            ));
        }
        self.advance();
        let then_branch = self.parse_or()?;
        if !matches!(self.peek().kind, TokenKind::Comma) {
            return Err(QueryError::new(
                self.peek().position,
                "expected `,` after `if` then-branch — `if` requires three arguments \
                 `if(COND, THEN, ELSE)`"
                    .into(),
            ));
        }
        self.advance();
        let else_branch = self.parse_or()?;
        if matches!(self.peek().kind, TokenKind::Comma) {
            return Err(QueryError::new(
                kw_pos,
                "`if` takes exactly three arguments `if(COND, THEN, ELSE)`".into(),
            ));
        }
        self.expect(&TokenKind::RParen, "expected `)` to close `if(...)`")?;
        Ok(Expr::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
        })
    }

    // ---- paths ----

    fn parse_path(&mut self, allow_field_set: bool) -> Result<PathExpr, QueryError> {
        let root = match &self.peek().kind {
            TokenKind::Dot => {
                self.advance();
                PathRoot::Identity
            }
            TokenKind::Ident(name) => {
                let n = name.clone();
                self.advance();
                PathRoot::Name(n)
            }
            _ => {
                return Err(QueryError::new(
                    self.peek().position,
                    "expected path".into(),
                ));
            }
        };

        let mut segments = Vec::new();

        if matches!(root, PathRoot::Identity) {
            if let Some(seg) = self.parse_first_identity_seg(allow_field_set)? {
                segments.push(seg);
            }
        }

        loop {
            match &self.peek().kind {
                TokenKind::Dot => {
                    self.advance();
                    let seg = self.parse_dot_segment(allow_field_set)?;
                    segments.push(seg);
                }
                TokenKind::LBrack => {
                    self.advance();
                    segments.push(self.parse_bracket_seg()?);
                }
                _ => break,
            }
        }

        Ok(PathExpr { root, segments })
    }

    fn parse_first_identity_seg(
        &mut self,
        allow_field_set: bool,
    ) -> Result<Option<PathSeg>, QueryError> {
        let kind = self.peek().kind.clone();
        match kind {
            TokenKind::Ident(name) => {
                self.advance();
                Ok(Some(PathSeg::Field(name)))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(Some(PathSeg::Field(s)))
            }
            TokenKind::StarStar => {
                self.advance();
                Ok(Some(PathSeg::StarStar))
            }
            TokenKind::LBrack => {
                self.advance();
                Ok(Some(self.parse_bracket_seg()?))
            }
            TokenKind::LBrace if allow_field_set => {
                Ok(Some(self.parse_field_set_seg()?))
            }
            _ => Ok(None),
        }
    }

    fn parse_dot_segment(&mut self, allow_field_set: bool) -> Result<PathSeg, QueryError> {
        let kind = self.peek().kind.clone();
        match kind {
            TokenKind::Ident(name) => {
                self.advance();
                Ok(PathSeg::Field(name))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(PathSeg::Field(s))
            }
            TokenKind::StarStar => {
                self.advance();
                Ok(PathSeg::StarStar)
            }
            TokenKind::LBrack => {
                self.advance();
                self.parse_bracket_seg()
            }
            TokenKind::LBrace if allow_field_set => self.parse_field_set_seg(),
            _ => Err(QueryError::new(
                self.peek().position,
                "expected identifier, `[`, `**`, or `{` after `.`".into(),
            )),
        }
    }

    /// Brackets carry one of: `[]` (iterate), `[N]` (numeric index),
    /// or `["field"]` (quoted field).
    fn parse_bracket_seg(&mut self) -> Result<PathSeg, QueryError> {
        match self.peek().kind.clone() {
            TokenKind::RBrack => {
                self.advance();
                Ok(PathSeg::Iterate)
            }
            TokenKind::Number(n) => {
                if n.fract() != 0.0 {
                    return Err(QueryError::new(
                        self.peek().position,
                        format!("array index must be an integer, got {}", n),
                    ));
                }
                let i = n as i64;
                self.advance();
                self.expect(&TokenKind::RBrack, "expected `]`")?;
                Ok(PathSeg::Index(i))
            }
            TokenKind::Str(s) => {
                self.advance();
                self.expect(&TokenKind::RBrack, "expected `]`")?;
                Ok(PathSeg::Field(s))
            }
            other => Err(QueryError::new(
                self.peek().position,
                format!(
                    "unexpected {} in brackets (expected `]`, an integer index, or a quoted field)",
                    other.description()
                ),
            )),
        }
    }

    fn parse_field_set_seg(&mut self) -> Result<PathSeg, QueryError> {
        self.advance();
        let mut items: Vec<FieldSetItem> = Vec::new();
        loop {
            if matches!(self.peek().kind, TokenKind::RBrace) {
                break;
            }
            // Spread `...NAME` — lexer surfaces as three Dot tokens.
            if matches!(self.peek().kind, TokenKind::Dot) {
                if let (Some(t1), Some(t2)) =
                    (self.tokens.get(self.pos + 1), self.tokens.get(self.pos + 2))
                {
                    if matches!(t1.kind, TokenKind::Dot) && matches!(t2.kind, TokenKind::Dot) {
                        self.advance();
                        self.advance();
                        self.advance();
                        let name_pos = self.peek().position;
                        match &self.peek().kind {
                            TokenKind::Ident(name) => {
                                items.push(FieldSetItem::Spread(name.clone()));
                                self.advance();
                            }
                            _ => {
                                return Err(QueryError::new(
                                    name_pos,
                                    "expected name after `...` in field-set".into(),
                                ));
                            }
                        }
                        if matches!(self.peek().kind, TokenKind::Comma) {
                            self.advance();
                            continue;
                        }
                        break;
                    }
                }
            }

            let name_pos = self.peek().position;
            let name = match &self.peek().kind {
                TokenKind::Ident(s) => s.clone(),
                TokenKind::Str(s) => s.clone(),
                _ => {
                    return Err(QueryError::new(
                        name_pos,
                        "expected field name in field-set".into(),
                    ));
                }
            };
            self.advance();
            if matches!(self.peek().kind, TokenKind::Colon) {
                self.advance();
                let value = self.parse_or()?;
                items.push(FieldSetItem::Override(name, value));
            } else {
                items.push(FieldSetItem::Field(name));
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RBrace, "expected `}`")?;
        if items.is_empty() {
            return Err(QueryError::new(
                self.peek().position,
                "field-set must contain at least one item".into(),
            ));
        }
        Ok(PathSeg::FieldSet(items))
    }
}
