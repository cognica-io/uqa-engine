//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boolean, comparison, arithmetic, unary, and postfix precedence parsing.

use super::{
    BinaryOp, CypherExpr, InList, IsNotNull, IsNull, ListIndex, ListSlice, Literal, ParseError,
    Parser, PropertyAccess, TokenKind, UnaryOp, Value,
};

impl Parser {
    pub(super) fn parse_expression(&mut self) -> Result<CypherExpr, ParseError> {
        let expression = self.with_expression_recursion(|parser| parser.parse_precedence(1))?;
        self.check_expression_depth(&expression)?;
        Ok(expression)
    }

    // Climbing precedence avoids retaining the entire precedence ladder for every nested atom. Each binary family is left-associative, including AGE's power and chained comparisons.
    fn parse_precedence(&mut self, minimum: u8) -> Result<CypherExpr, ParseError> {
        let mut left = if minimum <= 4 && self.match_keyword("NOT") {
            let operand = self.with_expression_recursion(|parser| parser.parse_precedence(4))?;
            CypherExpr::UnaryOp(UnaryOp {
                op: "NOT".into(),
                operand: Box::new(operand),
            })
        } else if self.match_kind(TokenKind::Minus).is_some() {
            let operand = self.with_expression_recursion(|parser| parser.parse_precedence(9))?;
            CypherExpr::UnaryOp(UnaryOp {
                op: "-".into(),
                operand: Box::new(operand),
            })
        } else {
            self.parse_postfix()?
        };
        loop {
            self.check_expression_depth(&left)?;
            let Some((precedence, operator)) = self.binary_operator() else {
                break;
            };
            if precedence < minimum {
                break;
            }
            self.advance();
            if operator == "IS" {
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
            if matches!(operator, "STARTS WITH" | "ENDS WITH") {
                self.expect_keyword("WITH")?;
            }
            let right = self.parse_precedence(precedence + 1)?;
            left = if operator == "IN" {
                CypherExpr::InList(InList {
                    expr: Box::new(left),
                    list_expr: Box::new(right),
                })
            } else {
                CypherExpr::BinaryOp(BinaryOp {
                    op: operator.into(),
                    left: Box::new(left),
                    right: Box::new(right),
                })
            };
        }
        Ok(left)
    }

    fn binary_operator(&self) -> Option<(u8, &'static str)> {
        for (keyword, precedence, operator) in [
            ("OR", 1, "OR"),
            ("XOR", 2, "XOR"),
            ("AND", 3, "AND"),
            ("IS", 5, "IS"),
            ("IN", 5, "IN"),
            ("STARTS", 5, "STARTS WITH"),
            ("ENDS", 5, "ENDS WITH"),
            ("CONTAINS", 5, "CONTAINS"),
        ] {
            if self.at_keyword(keyword) {
                return Some((precedence, operator));
            }
        }
        match self.peek().kind {
            TokenKind::Eq => Some((5, "=")),
            TokenKind::Neq => Some((5, "<>")),
            TokenKind::Lt => Some((5, "<")),
            TokenKind::Gt => Some((5, ">")),
            TokenKind::Lte => Some((5, "<=")),
            TokenKind::Gte => Some((5, ">=")),
            TokenKind::RegexMatch => Some((5, "=~")),
            TokenKind::Plus => Some((6, "+")),
            TokenKind::Minus => Some((6, "-")),
            TokenKind::Star => Some((7, "*")),
            TokenKind::Slash => Some((7, "/")),
            TokenKind::Percent => Some((7, "%")),
            TokenKind::Caret => Some((8, "^")),
            _ => None,
        }
    }

    pub(super) fn parse_postfix(&mut self) -> Result<CypherExpr, ParseError> {
        let mut expr = self.parse_atom()?;
        loop {
            self.check_expression_depth(&expr)?;
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
}
