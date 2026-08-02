//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full-text search query string parser. Rust implementation of
//! `uqa.sql.fts_query`.
//!
//! Grammar
//! -------
//! ```text
//!     query      = or_expr
//!     or_expr    = and_expr ( 'OR' and_expr )*
//!     and_expr   = unary ( ('AND' | <implicit>) unary )*
//!     unary      = 'NOT' unary | primary
//!     primary    = '(' or_expr ')'
//!                | TERM ':' PHRASE          -- field:"phrase"
//!                | TERM ':' VECTOR          -- field:[0.1, 0.2]
//!                | TERM ':' TERM            -- field:term
//!                | PHRASE                   -- "phrase"
//!                | TERM                     -- bare term
//! ```
//!
//! Operators AND / OR / NOT are case-insensitive keywords. Adjacent
//! terms without an explicit operator are treated as implicit AND.
//! Precedence: NOT > AND > OR.
//!
//! This module stops at the syntax AST. Physical retrieval lowering belongs
//! to the engine/planner boundary, so the SQL crate remains independent of
//! storage, scoring, fusion, and operator implementations.

use crate::error::{Result, SQLError};

// ---------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FTSTokenType {
    Term,
    Phrase,
    Vector,
    And,
    Or,
    Not,
    LParen,
    RParen,
    Colon,
    Eof,
}

#[derive(Debug, Clone)]
pub struct FTSToken {
    pub kind: FTSTokenType,
    pub value: String,
    pub pos: usize,
}

pub fn tokenize(source: &str) -> Result<Vec<FTSToken>> {
    let mut tokens: Vec<FTSToken> = Vec::new();
    let bytes: Vec<char> = source.chars().collect();
    let n = bytes.len();
    let mut i = 0_usize;

    while i < n {
        let ch = bytes[i];
        if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
            i += 1;
            continue;
        }
        match ch {
            '(' => {
                tokens.push(FTSToken {
                    kind: FTSTokenType::LParen,
                    value: "(".into(),
                    pos: i,
                });
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(FTSToken {
                    kind: FTSTokenType::RParen,
                    value: ")".into(),
                    pos: i,
                });
                i += 1;
                continue;
            }
            ':' => {
                tokens.push(FTSToken {
                    kind: FTSTokenType::Colon,
                    value: ":".into(),
                    pos: i,
                });
                i += 1;
                continue;
            }
            '"' => {
                let start = i;
                i += 1;
                let body_start = i;
                while i < n && bytes[i] != '"' {
                    i += 1;
                }
                if i >= n {
                    return Err(SQLError::TypeMismatch(format!(
                        "Unterminated quoted phrase starting at position {start}"
                    )));
                }
                let phrase: String = bytes[body_start..i].iter().collect();
                tokens.push(FTSToken {
                    kind: FTSTokenType::Phrase,
                    value: phrase,
                    pos: start,
                });
                i += 1;
                continue;
            }
            '[' => {
                let start = i;
                i += 1;
                let body_start = i;
                while i < n && bytes[i] != ']' {
                    i += 1;
                }
                if i >= n {
                    return Err(SQLError::TypeMismatch(format!(
                        "Unterminated vector literal starting at position {start}"
                    )));
                }
                let content: String = bytes[body_start..i].iter().collect();
                tokens.push(FTSToken {
                    kind: FTSTokenType::Vector,
                    value: content,
                    pos: start,
                });
                i += 1;
                continue;
            }
            _ => {}
        }
        if is_word_char(ch) {
            let start = i;
            while i < n && is_word_char(bytes[i]) {
                i += 1;
            }
            let word: String = bytes[start..i].iter().collect();
            let lower = word.to_ascii_lowercase();
            let kind = match lower.as_str() {
                "and" => FTSTokenType::And,
                "or" => FTSTokenType::Or,
                "not" => FTSTokenType::Not,
                _ => FTSTokenType::Term,
            };
            tokens.push(FTSToken {
                kind,
                value: word,
                pos: start,
            });
            continue;
        }
        return Err(SQLError::TypeMismatch(format!(
            "Unexpected character {ch:?} at position {i}"
        )));
    }

    tokens.push(FTSToken {
        kind: FTSTokenType::Eof,
        value: String::new(),
        pos: n,
    });
    Ok(tokens)
}

fn is_word_char(ch: char) -> bool {
    !matches!(
        ch,
        ' ' | '\t' | '\n' | '\r' | '(' | ')' | ':' | '"' | '[' | ']'
    )
}

// ---------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum FTSNode {
    Term {
        field: Option<String>,
        term: String,
    },
    Phrase {
        field: Option<String>,
        phrase: String,
    },
    Vector {
        field: Option<String>,
        values: Vec<f32>,
    },
    And(Box<FTSNode>, Box<FTSNode>),
    Or(Box<FTSNode>, Box<FTSNode>),
    Not(Box<FTSNode>),
}

// ---------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------

pub struct FTSParser {
    tokens: Vec<FTSToken>,
    pos: usize,
}

impl FTSParser {
    pub fn new(tokens: Vec<FTSToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(mut self) -> Result<FTSNode> {
        if self.peek().kind == FTSTokenType::Eof {
            return Err(SQLError::TypeMismatch("Empty query".into()));
        }
        let node = self.or_expr()?;
        if self.peek().kind != FTSTokenType::Eof {
            let tok = self.peek();
            return Err(SQLError::TypeMismatch(format!(
                "Unexpected token {:?} at position {}",
                tok.value, tok.pos
            )));
        }
        Ok(node)
    }

    fn peek(&self) -> &FTSToken {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> FTSToken {
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        tok
    }

    fn expect(&mut self, kind: FTSTokenType) -> Result<FTSToken> {
        let tok = self.advance();
        if tok.kind != kind {
            return Err(SQLError::TypeMismatch(format!(
                "Expected {:?}, got {:?} ({:?}) at position {}",
                kind, tok.kind, tok.value, tok.pos
            )));
        }
        Ok(tok)
    }

    fn or_expr(&mut self) -> Result<FTSNode> {
        let mut left = self.and_expr()?;
        while self.peek().kind == FTSTokenType::Or {
            self.advance();
            let right = self.and_expr()?;
            left = FTSNode::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<FTSNode> {
        let mut left = self.unary()?;
        loop {
            let kind = self.peek().kind;
            if kind == FTSTokenType::And {
                self.advance();
                let right = self.unary()?;
                left = FTSNode::And(Box::new(left), Box::new(right));
            } else if matches!(
                kind,
                FTSTokenType::Term
                    | FTSTokenType::Phrase
                    | FTSTokenType::Vector
                    | FTSTokenType::LParen
                    | FTSTokenType::Not
            ) {
                let right = self.unary()?;
                left = FTSNode::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<FTSNode> {
        if self.peek().kind == FTSTokenType::Not {
            self.advance();
            let operand = self.unary()?;
            return Ok(FTSNode::Not(Box::new(operand)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<FTSNode> {
        let kind = self.peek().kind;
        if kind == FTSTokenType::LParen {
            self.advance();
            let node = self.or_expr()?;
            self.expect(FTSTokenType::RParen)?;
            return Ok(node);
        }
        if kind == FTSTokenType::Phrase {
            let tok = self.advance();
            return Ok(FTSNode::Phrase {
                field: None,
                phrase: tok.value,
            });
        }
        if kind == FTSTokenType::Vector {
            let tok = self.advance();
            return Ok(FTSNode::Vector {
                field: None,
                values: parse_vector_literal(&tok.value)?,
            });
        }
        if kind == FTSTokenType::Term {
            let tok = self.advance();
            if self.peek().kind == FTSTokenType::Colon {
                self.advance();
                let next = self.peek().clone();
                match next.kind {
                    FTSTokenType::Phrase => {
                        self.advance();
                        return Ok(FTSNode::Phrase {
                            field: Some(tok.value),
                            phrase: next.value,
                        });
                    }
                    FTSTokenType::Vector => {
                        self.advance();
                        return Ok(FTSNode::Vector {
                            field: Some(tok.value),
                            values: parse_vector_literal(&next.value)?,
                        });
                    }
                    FTSTokenType::Term => {
                        self.advance();
                        return Ok(FTSNode::Term {
                            field: Some(tok.value),
                            term: next.value,
                        });
                    }
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "Expected term, phrase, or vector after ':', got {other:?} at position {}",
                            next.pos
                        )));
                    }
                }
            }
            return Ok(FTSNode::Term {
                field: None,
                term: tok.value,
            });
        }
        let tok = self.peek().clone();
        Err(SQLError::TypeMismatch(format!(
            "Unexpected token {:?} ({:?}) at position {}",
            tok.kind, tok.value, tok.pos
        )))
    }
}

fn parse_vector_literal(content: &str) -> Result<Vec<f32>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(SQLError::TypeMismatch("Empty vector literal".into()));
    }
    trimmed
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<f32>()
                .map_err(|e| SQLError::TypeMismatch(format!("Malformed vector literal: {e}")))
        })
        .collect()
}

/// Tokenize and parse a full-text query string without choosing a physical
/// retrieval representation.
pub fn parse_query_string(query_string: &str) -> Result<FTSNode> {
    let tokens = tokenize(query_string)?;
    FTSParser::new(tokens).parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_terms_and_phrase() {
        let tokens = tokenize(r#"hello "world today" foo"#).unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FTSTokenType::Term,
                FTSTokenType::Phrase,
                FTSTokenType::Term,
                FTSTokenType::Eof,
            ]
        );
        assert_eq!(tokens[1].value, "world today");
    }

    #[test]
    fn tokenize_vector() {
        let tokens = tokenize("[0.1, 0.2, 0.3]").unwrap();
        assert_eq!(tokens[0].kind, FTSTokenType::Vector);
        assert_eq!(tokens[0].value, "0.1, 0.2, 0.3");
    }

    #[test]
    fn parse_implicit_and() {
        let tokens = tokenize("rust ferris").unwrap();
        let ast = FTSParser::new(tokens).parse().unwrap();
        assert!(matches!(ast, FTSNode::And(_, _)));
    }

    #[test]
    fn parse_field_qualified_term() {
        let tokens = tokenize("title:rust").unwrap();
        let ast = FTSParser::new(tokens).parse().unwrap();
        match ast {
            FTSNode::Term { field, term } => {
                assert_eq!(field.as_deref(), Some("title"));
                assert_eq!(term, "rust");
            }
            _ => panic!("expected Term"),
        }
    }

    #[test]
    fn parse_not_precedence() {
        let tokens = tokenize("NOT foo OR bar").unwrap();
        let ast = FTSParser::new(tokens).parse().unwrap();
        // Should parse as (NOT foo) OR bar.
        match ast {
            FTSNode::Or(left, _) => match *left {
                FTSNode::Not(_) => {}
                _ => panic!("expected NOT on left"),
            },
            _ => panic!("expected OR"),
        }
    }

    #[test]
    fn parse_query_string_stops_at_syntax_ast() {
        let ast = parse_query_string("body:rust OR embedding:[1, 0]").unwrap();
        assert!(matches!(ast, FTSNode::Or(_, _)));
    }
}
