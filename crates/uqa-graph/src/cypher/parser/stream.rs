//! Token-stream lookahead, consumption, and expectation primitives.

use super::{is_keyword, ParseError, Parser, Token, TokenKind};

impl Parser {
    pub(super) fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    pub(super) fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        tok
    }

    pub(super) fn expect(
        &mut self,
        kind: TokenKind,
        label: &'static str,
    ) -> Result<Token, ParseError> {
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

    pub(super) fn match_kind(&mut self, kind: TokenKind) -> Option<Token> {
        if self.peek().kind == kind {
            Some(self.advance())
        } else {
            None
        }
    }

    pub(super) fn at_keyword(&self, keyword: &str) -> bool {
        is_keyword(self.peek(), keyword)
    }

    pub(super) fn match_keyword(&mut self, keyword: &str) -> bool {
        if self.at_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn expect_keyword(&mut self, keyword: &'static str) -> Result<Token, ParseError> {
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
}
