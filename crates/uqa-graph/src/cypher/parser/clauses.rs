//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level query clauses, projection, ordering, skip, and limit.

use super::{
    CreateClause, CypherClause, CypherExpr, CypherQuery, DeleteClause, MatchClause, MergeClause,
    OrderByItem, ParseError, Parser, ReturnClause, ReturnItem, SetClause, SetItem, SetOperator,
    TokenKind, UnwindClause, Variable, WithClause,
};

impl Parser {
    // -- Top-level -----------------------------------------------------

    pub(super) fn parse(&mut self) -> Result<CypherQuery, ParseError> {
        let mut clauses = Vec::new();
        while self.peek().kind != TokenKind::Eof {
            clauses.push(self.parse_clause()?);
        }
        Ok(CypherQuery { clauses })
    }

    pub(super) fn parse_clause(&mut self) -> Result<CypherClause, ParseError> {
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

    pub(super) fn parse_match(&mut self, optional: bool) -> Result<MatchClause, ParseError> {
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

    pub(super) fn parse_create(&mut self) -> Result<CreateClause, ParseError> {
        self.expect_keyword("CREATE")?;
        let patterns = self.parse_pattern_list()?;
        Ok(CreateClause { patterns })
    }

    pub(super) fn parse_merge(&mut self) -> Result<MergeClause, ParseError> {
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

    pub(super) fn parse_set(&mut self) -> Result<SetClause, ParseError> {
        self.expect_keyword("SET")?;
        let items = self.parse_set_items()?;
        Ok(SetClause { items })
    }

    pub(super) fn parse_set_items(&mut self) -> Result<Vec<SetItem>, ParseError> {
        let mut items = vec![self.parse_set_item()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            items.push(self.parse_set_item()?);
        }
        Ok(items)
    }

    pub(super) fn parse_set_item(&mut self) -> Result<SetItem, ParseError> {
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

    pub(super) fn parse_delete(&mut self, detach: bool) -> Result<DeleteClause, ParseError> {
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

    pub(super) fn parse_return(&mut self) -> Result<ReturnClause, ParseError> {
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

    pub(super) fn parse_with(&mut self) -> Result<WithClause, ParseError> {
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

    pub(super) fn parse_unwind(&mut self) -> Result<UnwindClause, ParseError> {
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

    pub(super) fn parse_return_items(&mut self) -> Result<Vec<ReturnItem>, ParseError> {
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

    pub(super) fn parse_return_item(&mut self) -> Result<ReturnItem, ParseError> {
        let expr = self.parse_expression()?;
        let alias = if self.match_keyword("AS") {
            Some(self.expect(TokenKind::Identifier, "alias")?.value)
        } else {
            None
        };
        Ok(ReturnItem { expr, alias })
    }

    pub(super) fn parse_order_by(&mut self) -> Result<Option<Vec<OrderByItem>>, ParseError> {
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

    pub(super) fn parse_order_item(&mut self) -> Result<OrderByItem, ParseError> {
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

    pub(super) fn parse_skip(&mut self) -> Result<Option<CypherExpr>, ParseError> {
        if self.match_keyword("SKIP") {
            Ok(Some(self.parse_expression()?))
        } else {
            Ok(None)
        }
    }

    pub(super) fn parse_limit(&mut self) -> Result<Option<CypherExpr>, ParseError> {
        if self.match_keyword("LIMIT") {
            Ok(Some(self.parse_expression()?))
        } else {
            Ok(None)
        }
    }
}
