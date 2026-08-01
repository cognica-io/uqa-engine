//! Path, node, relationship, variable-length, and property-map patterns.

use super::{
    BTreeMap, CypherExpr, NodePattern, ParseError, Parser, PathElement, PathPattern, RelDirection,
    RelPattern, TokenKind, RESERVED_KEYWORDS,
};

impl Parser {
    // -- Patterns ------------------------------------------------------

    pub(super) fn parse_pattern_list(&mut self) -> Result<Vec<PathPattern>, ParseError> {
        let mut patterns = vec![self.parse_path_pattern()?];
        while self.match_kind(TokenKind::Comma).is_some() {
            patterns.push(self.parse_path_pattern()?);
        }
        Ok(patterns)
    }

    pub(super) fn parse_path_pattern(&mut self) -> Result<PathPattern, ParseError> {
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

    pub(super) fn parse_node_pattern(&mut self) -> Result<NodePattern, ParseError> {
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

    pub(super) fn parse_rel_pattern(&mut self) -> Result<RelPattern, ParseError> {
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

    pub(super) fn parse_var_length(&mut self) -> Result<(Option<u32>, Option<u32>), ParseError> {
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

    pub(super) fn parse_property_map(
        &mut self,
    ) -> Result<BTreeMap<String, CypherExpr>, ParseError> {
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
}
