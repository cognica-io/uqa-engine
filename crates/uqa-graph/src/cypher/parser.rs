//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive-descent parser for the openCypher subset. Consumes
//! [`Token`]s from the lexer and produces a [`CypherQuery`] AST.

use std::collections::BTreeMap;

use uqa_core::Value;

use crate::cypher::ast::{
    BinaryOp, CaseExpr, CreateClause, CypherClause, CypherExpr, CypherQuery, DeleteClause,
    FunctionCall, InList, IsNotNull, IsNull, ListComprehension, ListIndex, ListLiteral, ListSlice,
    Literal, MapLiteral, MatchClause, MergeClause, NodePattern, OrderByItem, Parameter,
    PathElement, PathPattern, PropertyAccess, RelDirection, RelPattern, ReturnClause, ReturnItem,
    SetClause, SetItem, SetOperator, UnaryOp, UnwindClause, Variable, WithClause,
};
use crate::cypher::lexer::{is_keyword, tokenize, LexError, Token, TokenKind};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error(transparent)]
    Lex(#[from] LexError),
    #[error("expected {expected}, got {got:?} ({value:?}) at position {position}")]
    Expected {
        expected: &'static str,
        got: TokenKind,
        value: String,
        position: usize,
    },
    #[error("expected keyword {keyword:?}, got {got:?} at position {position}")]
    ExpectedKeyword {
        keyword: &'static str,
        got: String,
        position: usize,
    },
    #[error("unexpected token {got:?} at position {position}")]
    Unexpected { got: String, position: usize },
}

/// Parse a Cypher query string into a `CypherQuery` AST.
pub fn parse_cypher(source: &str) -> Result<CypherQuery, ParseError> {
    let tokens = tokenize(source)?;
    let mut p = Parser { tokens, pos: 0 };
    p.parse()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

const RESERVED_KEYWORDS: &[&str] = &[
    "AND",
    "AS",
    "ASC",
    "BY",
    "CASE",
    "CONTAINS",
    "CREATE",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "ELSE",
    "END",
    "ENDS",
    "EXISTS",
    "FALSE",
    "IN",
    "IS",
    "LIMIT",
    "MATCH",
    "MERGE",
    "NODE",
    "NOT",
    "NULL",
    "ON",
    "OPTIONAL",
    "OR",
    "ORDER",
    "RELATIONSHIP",
    "REMOVE",
    "RETURN",
    "SET",
    "SKIP",
    "STARTS",
    "THEN",
    "TRUE",
    "UNWIND",
    "WHEN",
    "WHERE",
    "WITH",
    "XOR",
];

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        tok
    }

    fn expect(&mut self, kind: TokenKind, label: &'static str) -> Result<Token, ParseError> {
        let tok = self.advance();
        if tok.kind != kind {
            return Err(ParseError::Expected {
                expected: label,
                got: tok.kind,
                value: tok.value,
                position: tok.pos,
            });
        }
        Ok(tok)
    }

    fn match_kind(&mut self, kind: TokenKind) -> Option<Token> {
        if self.peek().kind == kind {
            Some(self.advance())
        } else {
            None
        }
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        is_keyword(self.peek(), keyword)
    }

    fn match_keyword(&mut self, keyword: &str) -> bool {
        if self.at_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, keyword: &'static str) -> Result<Token, ParseError> {
        if self.match_keyword(keyword) {
            Ok(self.tokens[self.pos - 1].clone())
        } else {
            let tok = self.peek().clone();
            Err(ParseError::ExpectedKeyword {
                keyword,
                got: tok.value,
                position: tok.pos,
            })
        }
    }

    // -- Top-level -----------------------------------------------------

    fn parse(&mut self) -> Result<CypherQuery, ParseError> {
        let mut clauses = Vec::new();
        while self.peek().kind != TokenKind::Eof {
            clauses.push(self.parse_clause()?);
        }
        Ok(CypherQuery { clauses })
    }

    fn parse_clause(&mut self) -> Result<CypherClause, ParseError> {
        let tok = self.peek().clone();
        if tok.kind != TokenKind::Identifier {
            return Err(ParseError::Unexpected {
                got: tok.value,
                position: tok.pos,
            });
        }
        let kw = tok.value.to_uppercase();
        match kw.as_str() {
            "MATCH" => self.parse_match(false).map(CypherClause::Match),
            "OPTIONAL" => {
                self.advance();
                self.expect_keyword("MATCH")?;
                self.parse_match(true).map(CypherClause::Match)
            }
            "CREATE" => self.parse_create().map(CypherClause::Create),
            "MERGE" => self.parse_merge().map(CypherClause::Merge),
            "SET" => self.parse_set().map(CypherClause::Set),
            "DELETE" => self.parse_delete(false).map(CypherClause::Delete),
            "DETACH" => {
                self.advance();
                self.expect_keyword("DELETE")?;
                self.parse_delete(true).map(CypherClause::Delete)
            }
            "RETURN" => self.parse_return().map(CypherClause::Return),
            "WITH" => self.parse_with().map(CypherClause::With),
            "UNWIND" => self.parse_unwind().map(CypherClause::Unwind),
            _ => Err(ParseError::Unexpected {
                got: tok.value,
                position: tok.pos,
            }),
        }
    }

    // -- MATCH ---------------------------------------------------------

    fn parse_match(&mut self, optional: bool) -> Result<MatchClause, ParseError> {
        if !optional {
            self.expect_keyword("MATCH")?;
        }
        let patterns = self.parse_pattern_list()?;
        let r#where = if self.match_keyword("WHERE") {
            Some(self.parse_expression()?)
        } else {
            None
        };
        Ok(MatchClause {
            patterns,
            r#where,
            optional,
        })
    }

    fn parse_create(&mut self) -> Result<CreateClause, ParseError> {
        self.expect_keyword("CREATE")?;
        let patterns = self.parse_pattern_list()?;
        Ok(CreateClause { patterns })
    }

    fn parse_merge(&mut self) -> Result<MergeClause, ParseError> {
        self.expect_keyword("MERGE")?;
        let pattern = self.parse_path_pattern()?;
        let mut on_create = None;
        let mut on_match = None;
        while self.at_keyword("ON") {
            self.advance();
            if self.match_keyword("CREATE") {
                self.expect_keyword("SET")?;
                on_create = Some(self.parse_set_items()?);
            } else if self.match_keyword("MATCH") {
                self.expect_keyword("SET")?;
                on_match = Some(self.parse_set_items()?);
            } else {
                let tok = self.peek().clone();
                return Err(ParseError::Unexpected {
                    got: tok.value,
                    position: tok.pos,
                });
            }
        }
        Ok(MergeClause {
            pattern,
            on_create_set: on_create,
            on_match_set: on_match,
        })
    }

    fn parse_set(&mut self) -> Result<SetClause, ParseError> {
        self.expect_keyword("SET")?;
        let items = self.parse_set_items()?;
        Ok(SetClause { items })
    }

    fn parse_set_items(&mut self) -> Result<Vec<SetItem>, ParseError> {
        let mut items = vec![self.parse_set_item()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            items.push(self.parse_set_item()?);
        }
        Ok(items)
    }

    fn parse_set_item(&mut self) -> Result<SetItem, ParseError> {
        let target = self.parse_postfix()?;
        if self.match_kind(TokenKind::PlusEq).is_some() {
            let value = self.parse_expression()?;
            return Ok(SetItem {
                target,
                value,
                operator: SetOperator::Update,
            });
        }
        self.expect(TokenKind::Eq, "`=`")?;
        let value = self.parse_expression()?;
        Ok(SetItem {
            target,
            value,
            operator: SetOperator::Assign,
        })
    }

    fn parse_delete(&mut self, detach: bool) -> Result<DeleteClause, ParseError> {
        if !detach {
            self.expect_keyword("DELETE")?;
        }
        let mut exprs = vec![self.parse_expression()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            exprs.push(self.parse_expression()?);
        }
        Ok(DeleteClause {
            expressions: exprs,
            detach,
        })
    }

    fn parse_return(&mut self) -> Result<ReturnClause, ParseError> {
        self.expect_keyword("RETURN")?;
        let distinct = self.match_keyword("DISTINCT");
        let items = self.parse_return_items()?;
        let order_by = self.parse_order_by()?;
        let skip = self.parse_skip()?;
        let limit = self.parse_limit()?;
        Ok(ReturnClause {
            items,
            distinct,
            order_by,
            skip,
            limit,
        })
    }

    fn parse_with(&mut self) -> Result<WithClause, ParseError> {
        self.expect_keyword("WITH")?;
        let distinct = self.match_keyword("DISTINCT");
        let items = self.parse_return_items()?;
        let order_by = self.parse_order_by()?;
        let skip = self.parse_skip()?;
        let limit = self.parse_limit()?;
        let r#where = if self.match_keyword("WHERE") {
            Some(self.parse_expression()?)
        } else {
            None
        };
        Ok(WithClause {
            items,
            distinct,
            order_by,
            skip,
            limit,
            r#where,
        })
    }

    fn parse_unwind(&mut self) -> Result<UnwindClause, ParseError> {
        self.expect_keyword("UNWIND")?;
        let expr = self.parse_expression()?;
        self.expect_keyword("AS")?;
        let var = self.expect(TokenKind::Identifier, "variable")?.value;
        Ok(UnwindClause {
            expr,
            variable: var,
        })
    }

    // -- Shared return / with bits -------------------------------------

    fn parse_return_items(&mut self) -> Result<Vec<ReturnItem>, ParseError> {
        if self.peek().kind == TokenKind::Star {
            self.advance();
            return Ok(vec![ReturnItem {
                expr: CypherExpr::Variable(Variable {
                    name: "*".to_string(),
                }),
                alias: None,
            }]);
        }
        let mut items = vec![self.parse_return_item()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            items.push(self.parse_return_item()?);
        }
        Ok(items)
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, ParseError> {
        let expr = self.parse_expression()?;
        let alias = if self.match_keyword("AS") {
            Some(self.expect(TokenKind::Identifier, "alias")?.value)
        } else {
            None
        };
        Ok(ReturnItem { expr, alias })
    }

    fn parse_order_by(&mut self) -> Result<Option<Vec<OrderByItem>>, ParseError> {
        if !self.at_keyword("ORDER") {
            return Ok(None);
        }
        self.advance();
        self.expect_keyword("BY")?;
        let mut items = vec![self.parse_order_item()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            items.push(self.parse_order_item()?);
        }
        Ok(Some(items))
    }

    fn parse_order_item(&mut self) -> Result<OrderByItem, ParseError> {
        let expr = self.parse_expression()?;
        let ascending = if self.match_keyword("DESC") {
            false
        } else {
            // ASC is the default; consume the keyword if present.
            self.match_keyword("ASC");
            true
        };
        Ok(OrderByItem { expr, ascending })
    }

    fn parse_skip(&mut self) -> Result<Option<CypherExpr>, ParseError> {
        if self.match_keyword("SKIP") {
            Ok(Some(self.parse_expression()?))
        } else {
            Ok(None)
        }
    }

    fn parse_limit(&mut self) -> Result<Option<CypherExpr>, ParseError> {
        if self.match_keyword("LIMIT") {
            Ok(Some(self.parse_expression()?))
        } else {
            Ok(None)
        }
    }

    // -- Patterns ------------------------------------------------------

    fn parse_pattern_list(&mut self) -> Result<Vec<PathPattern>, ParseError> {
        let mut patterns = vec![self.parse_path_pattern()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            patterns.push(self.parse_path_pattern()?);
        }
        Ok(patterns)
    }

    fn parse_path_pattern(&mut self) -> Result<PathPattern, ParseError> {
        // Optional path variable: `p = (a)-[...]->(b)`.
        let mut variable = None;
        if self.peek().kind == TokenKind::Identifier
            && self.pos + 1 < self.tokens.len()
            && self.tokens[self.pos + 1].kind == TokenKind::Eq
        {
            variable = Some(self.advance().value);
            self.expect(TokenKind::Eq, "`=`")?;
        }
        let mut elements = vec![PathElement::Node(self.parse_node_pattern()?)];
        while matches!(
            self.peek().kind,
            TokenKind::Minus | TokenKind::Lt | TokenKind::ArrowLeft
        ) {
            let rel = self.parse_rel_pattern()?;
            elements.push(PathElement::Rel(rel));
            elements.push(PathElement::Node(self.parse_node_pattern()?));
        }
        Ok(PathPattern { variable, elements })
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern, ParseError> {
        self.expect(TokenKind::LParen, "`(`")?;
        let mut variable = None;
        let mut labels = Vec::new();
        let mut properties = None;
        if self.peek().kind == TokenKind::Identifier && !self.at_keyword("WHERE") {
            variable = Some(self.advance().value);
        }
        while self.peek().kind == TokenKind::Colon {
            self.advance();
            labels.push(self.expect(TokenKind::Identifier, "label")?.value);
        }
        if self.peek().kind == TokenKind::LBrace {
            properties = Some(self.parse_property_map()?);
        }
        self.expect(TokenKind::RParen, "`)`")?;
        Ok(NodePattern {
            variable,
            labels,
            properties,
        })
    }

    fn parse_rel_pattern(&mut self) -> Result<RelPattern, ParseError> {
        let left_arrow = if self.match_kind(TokenKind::ArrowLeft).is_some() {
            true
        } else if self.match_kind(TokenKind::Minus).is_some() {
            false
        } else {
            let tok = self.peek().clone();
            return Err(ParseError::Expected {
                expected: "`-` or `<-`",
                got: tok.kind,
                value: tok.value,
                position: tok.pos,
            });
        };

        let mut variable = None;
        let mut types = Vec::new();
        let mut properties = None;
        let mut min_hops = None;
        let mut max_hops = None;

        if self.match_kind(TokenKind::LBracket).is_some() {
            if self.peek().kind == TokenKind::Identifier
                && !RESERVED_KEYWORDS.contains(&self.peek().value.to_uppercase().as_str())
            {
                variable = Some(self.advance().value);
            }
            if self.peek().kind == TokenKind::Colon {
                self.advance();
                types.push(self.expect(TokenKind::Identifier, "type")?.value);
                while self.match_kind(TokenKind::Pipe).is_some() {
                    types.push(self.expect(TokenKind::Identifier, "type")?.value);
                }
            }
            if self.match_kind(TokenKind::Star).is_some() {
                let (mn, mx) = self.parse_var_length()?;
                min_hops = mn;
                max_hops = mx;
            }
            if self.peek().kind == TokenKind::LBrace {
                properties = Some(self.parse_property_map()?);
            }
            self.expect(TokenKind::RBracket, "`]`")?;
        }

        let direction = if left_arrow {
            self.expect(TokenKind::Minus, "`-`")?;
            RelDirection::Left
        } else if self.match_kind(TokenKind::ArrowRight).is_some() {
            RelDirection::Right
        } else if self.match_kind(TokenKind::Minus).is_some() {
            RelDirection::Both
        } else {
            let tok = self.peek().clone();
            return Err(ParseError::Expected {
                expected: "`->` or `-`",
                got: tok.kind,
                value: tok.value,
                position: tok.pos,
            });
        };

        Ok(RelPattern {
            variable,
            types,
            properties,
            direction,
            min_hops,
            max_hops,
        })
    }

    fn parse_var_length(&mut self) -> Result<(Option<u32>, Option<u32>), ParseError> {
        // Forms: `*`, `*N`, `*N..M`, `*..M`, `*N..`.
        let mut min_hops = None;
        let mut max_hops = None;
        if self.peek().kind == TokenKind::Integer {
            let n = self
                .advance()
                .value
                .parse::<u32>()
                .map_err(|_| ParseError::Unexpected {
                    got: "non-integer hop count".into(),
                    position: 0,
                })?;
            min_hops = Some(n);
            if self.match_kind(TokenKind::DotDot).is_some() {
                if self.peek().kind == TokenKind::Integer {
                    let m = self.advance().value.parse::<u32>().map_err(|_| {
                        ParseError::Unexpected {
                            got: "non-integer hop count".into(),
                            position: 0,
                        }
                    })?;
                    max_hops = Some(m);
                }
            } else {
                max_hops = Some(n);
            }
        } else if self.match_kind(TokenKind::DotDot).is_some() {
            if self.peek().kind == TokenKind::Integer {
                let m =
                    self.advance()
                        .value
                        .parse::<u32>()
                        .map_err(|_| ParseError::Unexpected {
                            got: "non-integer hop count".into(),
                            position: 0,
                        })?;
                max_hops = Some(m);
            }
        } else {
            min_hops = Some(1);
        }
        Ok((min_hops, max_hops))
    }

    fn parse_property_map(&mut self) -> Result<BTreeMap<String, CypherExpr>, ParseError> {
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut map = BTreeMap::new();
        if self.peek().kind != TokenKind::RBrace {
            let key = self.expect(TokenKind::Identifier, "key")?.value;
            self.expect(TokenKind::Colon, "`:`")?;
            let value = self.parse_expression()?;
            map.insert(key, value);
            while self.match_kind(TokenKind::Comma).is_some() {
                let key = self.expect(TokenKind::Identifier, "key")?.value;
                self.expect(TokenKind::Colon, "`:`")?;
                let value = self.parse_expression()?;
                map.insert(key, value);
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(map)
    }

    // -- Expressions --------------------------------------------------

    fn parse_expression(&mut self) -> Result<CypherExpr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_xor()?;
        while self.match_keyword("OR") {
            let right = self.parse_xor()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op: "OR".into(),
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_xor(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_and()?;
        while self.match_keyword("XOR") {
            let right = self.parse_and()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op: "XOR".into(),
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_not()?;
        while self.match_keyword("AND") {
            let right = self.parse_not()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op: "AND".into(),
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<CypherExpr, ParseError> {
        if self.match_keyword("NOT") {
            let operand = self.parse_not()?;
            return Ok(CypherExpr::UnaryOp(UnaryOp {
                op: "NOT".into(),
                operand: Box::new(operand),
            }));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_addition()?;

        loop {
            if self.match_keyword("IS") {
                left = if self.match_keyword("NOT") {
                    self.expect_keyword("NULL")?;
                    CypherExpr::IsNotNull(IsNotNull {
                        expr: Box::new(left),
                    })
                } else {
                    self.expect_keyword("NULL")?;
                    CypherExpr::IsNull(IsNull {
                        expr: Box::new(left),
                    })
                };
                continue;
            }

            if self.match_keyword("IN") {
                let right = self.parse_addition()?;
                left = CypherExpr::InList(InList {
                    expr: Box::new(left),
                    list_expr: Box::new(right),
                });
                continue;
            }

            if self.match_keyword("STARTS") {
                self.expect_keyword("WITH")?;
                let right = self.parse_addition()?;
                left = CypherExpr::BinaryOp(BinaryOp {
                    op: "STARTS WITH".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                });
                continue;
            }
            if self.match_keyword("ENDS") {
                self.expect_keyword("WITH")?;
                let right = self.parse_addition()?;
                left = CypherExpr::BinaryOp(BinaryOp {
                    op: "ENDS WITH".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                });
                continue;
            }
            if self.match_keyword("CONTAINS") {
                let right = self.parse_addition()?;
                left = CypherExpr::BinaryOp(BinaryOp {
                    op: "CONTAINS".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                });
                continue;
            }

            // `=~` regular-expression operator.
            if self.match_kind(TokenKind::RegexMatch).is_some() {
                let right = self.parse_addition()?;
                left = CypherExpr::BinaryOp(BinaryOp {
                    op: "=~".into(),
                    left: Box::new(left),
                    right: Box::new(right),
                });
                continue;
            }

            let op = match self.peek().kind {
                TokenKind::Eq => Some("="),
                TokenKind::Neq => Some("<>"),
                TokenKind::Lt => Some("<"),
                TokenKind::Gt => Some(">"),
                TokenKind::Lte => Some("<="),
                TokenKind::Gte => Some(">="),
                _ => None,
            };
            let Some(op) = op else {
                break;
            };
            // Comparisons chain left-associatively in AGE:
            // `1 < 2 < 3` evaluates as `(1 < 2) < 3`.
            self.advance();
            let right = self.parse_addition()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op: op.into(),
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_addition(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_multiplication()?;
        while matches!(self.peek().kind, TokenKind::Plus | TokenKind::Minus) {
            let op = self.advance().value;
            let right = self.parse_multiplication()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<CypherExpr, ParseError> {
        let mut left = self.parse_power()?;
        while matches!(
            self.peek().kind,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent
        ) {
            let op = self.advance().value;
            let right = self.parse_power()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_power(&mut self) -> Result<CypherExpr, ParseError> {
        // `^` binds tighter than `*` but looser than unary minus and
        // chains left-associatively (AGE: `-2^2` = 4.0, `2^3^2` = 64.0).
        let mut left = self.parse_unary()?;
        while self.peek().kind == TokenKind::Caret {
            self.advance();
            let right = self.parse_unary()?;
            left = CypherExpr::BinaryOp(BinaryOp {
                op: "^".into(),
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<CypherExpr, ParseError> {
        if self.peek().kind == TokenKind::Minus {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(CypherExpr::UnaryOp(UnaryOp {
                op: "-".into(),
                operand: Box::new(operand),
            }));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<CypherExpr, ParseError> {
        let mut expr = self.parse_atom()?;
        loop {
            if self.peek().kind == TokenKind::Dot {
                self.advance();
                let key = self.expect(TokenKind::Identifier, "property name")?.value;
                expr = match expr {
                    CypherExpr::Variable(v) => CypherExpr::PropertyAccess(PropertyAccess {
                        variable: v.name,
                        keys: vec![key],
                    }),
                    CypherExpr::PropertyAccess(mut pa) => {
                        pa.keys.push(key);
                        CypherExpr::PropertyAccess(pa)
                    }
                    other => CypherExpr::ListIndex(ListIndex {
                        expr: Box::new(other),
                        index: Box::new(CypherExpr::Literal(Literal {
                            value: Value::Str(key),
                        })),
                    }),
                };
            } else if self.peek().kind == TokenKind::LBracket {
                self.advance();
                // Slice or index. Forms: `[i]`, `[a..b]`, `[..b]`,
                // `[a..]`, `[..]`.
                if self.peek().kind == TokenKind::DotDot {
                    self.advance();
                    let end = if self.peek().kind == TokenKind::RBracket {
                        None
                    } else {
                        Some(Box::new(self.parse_expression()?))
                    };
                    self.expect(TokenKind::RBracket, "`]`")?;
                    expr = CypherExpr::ListSlice(ListSlice {
                        expr: Box::new(expr),
                        start: None,
                        end,
                    });
                    continue;
                }
                let index = self.parse_expression()?;
                if self.match_kind(TokenKind::DotDot).is_some() {
                    let end = if self.peek().kind == TokenKind::RBracket {
                        None
                    } else {
                        Some(Box::new(self.parse_expression()?))
                    };
                    self.expect(TokenKind::RBracket, "`]`")?;
                    expr = CypherExpr::ListSlice(ListSlice {
                        expr: Box::new(expr),
                        start: Some(Box::new(index)),
                        end,
                    });
                    continue;
                }
                self.expect(TokenKind::RBracket, "`]`")?;
                expr = CypherExpr::ListIndex(ListIndex {
                    expr: Box::new(expr),
                    index: Box::new(index),
                });
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_atom(&mut self) -> Result<CypherExpr, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(TokenKind::RParen, "`)`")?;
                Ok(expr)
            }
            TokenKind::LBracket => self.parse_list_literal(),
            TokenKind::LBrace => self.parse_map_literal(),
            TokenKind::Dollar => {
                self.advance();
                let name = self.expect(TokenKind::Identifier, "parameter name")?.value;
                Ok(CypherExpr::Parameter(Parameter { name }))
            }
            TokenKind::Integer => {
                self.advance();
                let n = tok
                    .value
                    .parse::<i64>()
                    .map_err(|_| ParseError::Unexpected {
                        got: tok.value.clone(),
                        position: tok.pos,
                    })?;
                Ok(CypherExpr::Literal(Literal {
                    value: Value::Int(n),
                }))
            }
            TokenKind::Float => {
                self.advance();
                let f = tok
                    .value
                    .parse::<f64>()
                    .map_err(|_| ParseError::Unexpected {
                        got: tok.value.clone(),
                        position: tok.pos,
                    })?;
                Ok(CypherExpr::Literal(Literal {
                    value: Value::Float(f),
                }))
            }
            TokenKind::String => {
                self.advance();
                Ok(CypherExpr::Literal(Literal {
                    value: Value::Str(tok.value),
                }))
            }
            TokenKind::Identifier => {
                let upper = tok.value.to_uppercase();
                match upper.as_str() {
                    "TRUE" => {
                        self.advance();
                        Ok(CypherExpr::Literal(Literal {
                            value: Value::Bool(true),
                        }))
                    }
                    "FALSE" => {
                        self.advance();
                        Ok(CypherExpr::Literal(Literal {
                            value: Value::Bool(false),
                        }))
                    }
                    "NULL" => {
                        self.advance();
                        Ok(CypherExpr::Literal(Literal { value: Value::Null }))
                    }
                    "CASE" => self.parse_case().map(CypherExpr::CaseExpr),
                    _ => {
                        if self.pos + 1 < self.tokens.len()
                            && self.tokens[self.pos + 1].kind == TokenKind::LParen
                        {
                            // `exists((a)-[:R]->(b))` takes a path
                            // pattern, not an expression list.
                            if upper == "EXISTS"
                                && self.pos + 2 < self.tokens.len()
                                && self.tokens[self.pos + 2].kind == TokenKind::LParen
                            {
                                self.advance();
                                self.expect(TokenKind::LParen, "`(`")?;
                                let pattern = self.parse_path_pattern()?;
                                self.expect(TokenKind::RParen, "`)`")?;
                                return Ok(CypherExpr::ExistsPattern(pattern));
                            }
                            self.parse_function_call().map(CypherExpr::FunctionCall)
                        } else {
                            self.advance();
                            Ok(CypherExpr::Variable(Variable { name: tok.value }))
                        }
                    }
                }
            }
            _ => Err(ParseError::Unexpected {
                got: tok.value,
                position: tok.pos,
            }),
        }
    }

    fn parse_list_literal(&mut self) -> Result<CypherExpr, ParseError> {
        self.expect(TokenKind::LBracket, "`[`")?;
        // `[x IN list WHERE pred | map]` is a list comprehension.
        if self.peek().kind == TokenKind::Identifier
            && !RESERVED_KEYWORDS.contains(&self.peek().value.to_uppercase().as_str())
            && self.pos + 1 < self.tokens.len()
            && is_keyword(&self.tokens[self.pos + 1], "IN")
        {
            let variable = self.advance().value;
            self.expect_keyword("IN")?;
            let list_expr = self.parse_expression()?;
            let filter = if self.match_keyword("WHERE") {
                Some(Box::new(self.parse_expression()?))
            } else {
                None
            };
            let map_expr = if self.match_kind(TokenKind::Pipe).is_some() {
                Some(Box::new(self.parse_expression()?))
            } else {
                None
            };
            self.expect(TokenKind::RBracket, "`]`")?;
            return Ok(CypherExpr::ListComprehension(ListComprehension {
                variable,
                list_expr: Box::new(list_expr),
                filter,
                map_expr,
            }));
        }
        let mut elements = Vec::new();
        if self.peek().kind != TokenKind::RBracket {
            elements.push(self.parse_expression()?);
            while self.match_kind(TokenKind::Comma).is_some() {
                elements.push(self.parse_expression()?);
            }
        }
        self.expect(TokenKind::RBracket, "`]`")?;
        Ok(CypherExpr::ListLiteral(ListLiteral { elements }))
    }

    fn parse_map_literal(&mut self) -> Result<CypherExpr, ParseError> {
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut pairs: Vec<(String, CypherExpr)> = Vec::new();
        if self.peek().kind != TokenKind::RBrace {
            loop {
                let key_tok = self.advance();
                let key = match key_tok.kind {
                    TokenKind::Identifier | TokenKind::String => key_tok.value,
                    _ => {
                        return Err(ParseError::Expected {
                            expected: "map key",
                            got: key_tok.kind,
                            value: key_tok.value,
                            position: key_tok.pos,
                        });
                    }
                };
                self.expect(TokenKind::Colon, "`:`")?;
                let value = self.parse_expression()?;
                pairs.push((key, value));
                if self.match_kind(TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(CypherExpr::MapLiteral(MapLiteral { pairs }))
    }

    fn parse_function_call(&mut self) -> Result<FunctionCall, ParseError> {
        let name = self.advance().value;
        self.expect(TokenKind::LParen, "`(`")?;
        let distinct = self.match_keyword("DISTINCT");
        let mut args = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            if self.peek().kind == TokenKind::Star {
                self.advance();
                args.push(CypherExpr::Variable(Variable {
                    name: "*".to_string(),
                }));
            } else {
                args.push(self.parse_expression()?);
                while self.match_kind(TokenKind::Comma).is_some() {
                    args.push(self.parse_expression()?);
                }
            }
        }
        self.expect(TokenKind::RParen, "`)`")?;
        Ok(FunctionCall {
            name,
            args,
            distinct,
        })
    }

    fn parse_case(&mut self) -> Result<CaseExpr, ParseError> {
        self.expect_keyword("CASE")?;
        let operand = if self.at_keyword("WHEN") {
            None
        } else {
            Some(Box::new(self.parse_expression()?))
        };
        let mut whens = Vec::new();
        while self.match_keyword("WHEN") {
            let cond = self.parse_expression()?;
            self.expect_keyword("THEN")?;
            let result = self.parse_expression()?;
            whens.push((cond, result));
        }
        let else_expr = if self.match_keyword("ELSE") {
            Some(Box::new(self.parse_expression()?))
        } else {
            None
        };
        self.expect_keyword("END")?;
        Ok(CaseExpr {
            operand,
            whens,
            else_expr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_match_return() {
        let q = parse_cypher("MATCH (n:Person) RETURN n.name").unwrap();
        assert_eq!(q.clauses.len(), 2);
        match &q.clauses[0] {
            CypherClause::Match(m) => {
                assert!(!m.optional);
                assert_eq!(m.patterns.len(), 1);
                let elements = &m.patterns[0].elements;
                assert_eq!(elements.len(), 1);
                if let PathElement::Node(np) = &elements[0] {
                    assert_eq!(np.variable.as_deref(), Some("n"));
                    assert_eq!(np.labels, vec!["Person".to_string()]);
                } else {
                    panic!("expected node pattern");
                }
            }
            _ => panic!("expected MATCH"),
        }
        match &q.clauses[1] {
            CypherClause::Return(r) => {
                assert_eq!(r.items.len(), 1);
                if let CypherExpr::PropertyAccess(pa) = &r.items[0].expr {
                    assert_eq!(pa.variable, "n");
                    assert_eq!(pa.keys, vec!["name".to_string()]);
                } else {
                    panic!("expected PropertyAccess");
                }
            }
            _ => panic!("expected RETURN"),
        }
    }

    #[test]
    fn parse_match_with_relationship() {
        let q = parse_cypher("MATCH (a)-[r:KNOWS]->(b) RETURN b").unwrap();
        if let CypherClause::Match(m) = &q.clauses[0] {
            let path = &m.patterns[0];
            assert_eq!(path.elements.len(), 3);
            if let PathElement::Rel(rp) = &path.elements[1] {
                assert_eq!(rp.variable.as_deref(), Some("r"));
                assert_eq!(rp.types, vec!["KNOWS".to_string()]);
                assert_eq!(rp.direction, RelDirection::Right);
            } else {
                panic!("expected rel pattern");
            }
        } else {
            panic!("expected MATCH");
        }
    }

    #[test]
    fn parse_variable_length_path() {
        let q = parse_cypher("MATCH (a)-[*1..3]->(b) RETURN b").unwrap();
        if let CypherClause::Match(m) = &q.clauses[0] {
            if let PathElement::Rel(rp) = &m.patterns[0].elements[1] {
                assert_eq!(rp.min_hops, Some(1));
                assert_eq!(rp.max_hops, Some(3));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn parse_where_with_comparison_and_logic() {
        let q = parse_cypher("MATCH (n) WHERE n.age > 18 AND n.name = 'a' RETURN n").unwrap();
        if let CypherClause::Match(m) = &q.clauses[0] {
            assert!(m.r#where.is_some());
            if let CypherExpr::BinaryOp(bo) = m.r#where.as_ref().unwrap() {
                assert_eq!(bo.op, "AND");
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn parse_function_call_and_distinct() {
        let q = parse_cypher("MATCH (n) RETURN count(DISTINCT n.id)").unwrap();
        if let CypherClause::Return(r) = &q.clauses[1] {
            if let CypherExpr::FunctionCall(fc) = &r.items[0].expr {
                assert_eq!(fc.name, "count");
                assert!(fc.distinct);
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn parse_create_with_property_map() {
        let q = parse_cypher("CREATE (n:Person {name: 'alice', age: 30})").unwrap();
        if let CypherClause::Create(c) = &q.clauses[0] {
            if let PathElement::Node(np) = &c.patterns[0].elements[0] {
                let props = np.properties.as_ref().unwrap();
                assert!(props.contains_key("name"));
                assert!(props.contains_key("age"));
            } else {
                panic!();
            }
        }
    }

    #[test]
    fn parse_unwind_and_with() {
        let q = parse_cypher("UNWIND [1,2,3] AS x WITH x WHERE x > 1 RETURN x").unwrap();
        assert_eq!(q.clauses.len(), 3);
        if let CypherClause::Unwind(u) = &q.clauses[0] {
            assert_eq!(u.variable, "x");
        } else {
            panic!();
        }
        if let CypherClause::With(w) = &q.clauses[1] {
            assert!(w.r#where.is_some());
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_optional_match_and_set() {
        let q = parse_cypher("OPTIONAL MATCH (n) SET n.flag = true RETURN n").unwrap();
        if let CypherClause::Match(m) = &q.clauses[0] {
            assert!(m.optional);
        } else {
            panic!();
        }
        if let CypherClause::Set(s) = &q.clauses[1] {
            assert_eq!(s.items[0].operator, SetOperator::Assign);
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_detach_delete() {
        let q = parse_cypher("MATCH (n) DETACH DELETE n").unwrap();
        if let CypherClause::Delete(d) = &q.clauses[1] {
            assert!(d.detach);
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_in_list_and_starts_with() {
        let q = parse_cypher("MATCH (n) WHERE n.name STARTS WITH 'a' AND n.id IN [1, 2] RETURN n")
            .unwrap();
        if let CypherClause::Match(m) = &q.clauses[0] {
            assert!(m.r#where.is_some());
        } else {
            panic!();
        }
    }
}
