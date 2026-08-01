//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literals, comprehensions, maps, calls, parameters, and CASE expressions.

use super::{
    is_keyword, CaseExpr, CypherExpr, FunctionCall, ListComprehension, ListLiteral, Literal,
    MapLiteral, Parameter, ParseError, Parser, TokenKind, Value, Variable, RESERVED_KEYWORDS,
};

impl Parser {
    pub(super) fn parse_atom(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_list_literal(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_map_literal(&mut self) -> Result<CypherExpr, ParseError> {
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

    pub(super) fn parse_function_call(&mut self) -> Result<FunctionCall, ParseError> {
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

    pub(super) fn parse_case(&mut self) -> Result<CaseExpr, ParseError> {
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
