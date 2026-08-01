//! Boolean, comparison, arithmetic, unary, and postfix precedence parsing.

use super::{
    BinaryOp, CypherExpr, InList, IsNotNull, IsNull, ListIndex, ListSlice, Literal, ParseError,
    Parser, PropertyAccess, TokenKind, UnaryOp, Value,
};

impl Parser {
    pub(super) fn parse_expression(&mut self) -> Result<CypherExpr, ParseError> {
        self.parse_or()
    }

    pub(super) fn parse_or(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_xor(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_and(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_not(&mut self) -> Result<CypherExpr, ParseError> {
        if self.match_keyword("NOT") {
            let operand = self.parse_not()?;
            return Ok(CypherExpr::UnaryOp(UnaryOp {
                op: "NOT".into(),
                operand: Box::new(operand),
            }));
        }
        self.parse_comparison()
    }

    pub(super) fn parse_comparison(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_addition(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_multiplication(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_power(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_unary(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_postfix(&mut self) -> Result<CypherExpr, ParseError> {
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
}
