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
//! `compile` lowers the AST into an [`OperatorTree`]. Term and phrase
//! nodes become `Term` / `Intersect(Term*)` (phrases are tokenized
//! into individual terms by the caller-supplied phrase tokenizer);
//! vectors become `KNN { k = 10_000 }`; AND uses robust positive-evidence
//! pooling when one side has a vector signal and the other does not.

use crate::error::{Result, SQLError};
use uqa_operators::{GatingSpec, OperatorTree};

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

// ---------------------------------------------------------------------
// AST -> OperatorTree compilation
// ---------------------------------------------------------------------

/// Pluggable hook for tokenizing a phrase into individual terms. The
/// parser doesn't know about analyzers, so the caller (the SQL
/// compiler in the engine) wires this through. Returns the analyzed
/// tokens used to construct an `Intersect(Term*)` retrieval.
pub type PhraseTokenizer<'a> =
    dyn Fn(/*field*/ Option<&str>, /*phrase*/ &str) -> Vec<String> + 'a;

/// Resolve `_all` to `None` (all-field search). Mirrors
/// `_resolve_field`.
pub fn resolve_field(node_field: Option<&str>, default_field: Option<&str>) -> Option<String> {
    let field = node_field.or(default_field)?;
    if field == "_all" {
        None
    } else {
        Some(field.to_string())
    }
}

/// Default `k` used by the compiled vector node. Mirrors
/// `_CalibratedKNNOperator(query_vec, k=10000, field=field)` in the
/// canonical UQA behavior.
pub const FTS_VECTOR_K: usize = 10_000;

/// Lower an [`FTSNode`] into an [`OperatorTree`]. The phrase
/// tokenizer is called for every `PhraseNode`; if it returns an empty
/// vector the result is the `Empty` operator.
pub fn compile(
    node: &FTSNode,
    default_field: Option<&str>,
    phrase_tokenizer: &PhraseTokenizer<'_>,
) -> OperatorTree {
    match node {
        FTSNode::Term { field, term } => {
            let resolved = resolve_field(field.as_deref(), default_field);
            OperatorTree::Term {
                query: term.clone(),
                field: resolved,
                scoring: None,
            }
        }
        FTSNode::Phrase { field, phrase } => {
            let resolved = resolve_field(field.as_deref(), default_field);
            let terms = phrase_tokenizer(resolved.as_deref(), phrase);
            if terms.is_empty() {
                return OperatorTree::Empty;
            }
            if terms.len() == 1 {
                let Some(query) = terms.into_iter().next() else {
                    return OperatorTree::Empty;
                };
                return OperatorTree::Term {
                    query,
                    field: resolved,
                    scoring: None,
                };
            }
            OperatorTree::Intersect(
                terms
                    .into_iter()
                    .map(|t| OperatorTree::Term {
                        query: t,
                        field: resolved.clone(),
                        scoring: None,
                    })
                    .collect(),
            )
        }
        FTSNode::Vector { field, values } => {
            let resolved = resolve_field(field.as_deref(), default_field)
                .unwrap_or_else(|| "embedding".into());
            OperatorTree::KNN {
                query_vector: values.clone(),
                k: FTS_VECTOR_K,
                field: resolved,
            }
        }
        FTSNode::And(left, right) => {
            let l = compile(left, default_field, phrase_tokenizer);
            let r = compile(right, default_field, phrase_tokenizer);
            // Mixed text + vector AND -> robust positive-evidence pooling
            // (same shape as the canonical UQA implementation's `_compile_and`).
            let mixed = has_vector_signal(left) ^ has_vector_signal(right);
            if mixed {
                OperatorTree::RobustPositiveEvidencePool {
                    signals: vec![l, r],
                    alpha: 0.5,
                    gating: GatingSpec::Softplus,
                    weights: None,
                    logit_min: None,
                    logit_max: None,
                    adaptive_weights: false,
                }
            } else {
                OperatorTree::Intersect(vec![l, r])
            }
        }
        FTSNode::Or(left, right) => OperatorTree::Union(vec![
            compile(left, default_field, phrase_tokenizer),
            compile(right, default_field, phrase_tokenizer),
        ]),
        FTSNode::Not(operand) => {
            OperatorTree::Complement(Box::new(compile(operand, default_field, phrase_tokenizer)))
        }
    }
}

/// Convenience: tokenize, parse, compile in one call. Returns the
/// resulting [`OperatorTree`].
pub fn compile_query_string(
    query_string: &str,
    default_field: Option<&str>,
    phrase_tokenizer: &PhraseTokenizer<'_>,
) -> Result<OperatorTree> {
    let tokens = tokenize(query_string)?;
    let ast = FTSParser::new(tokens).parse()?;
    Ok(compile(&ast, default_field, phrase_tokenizer))
}

/// Returns `true` when the AST subtree contains any `VectorNode`.
pub fn has_vector_signal(node: &FTSNode) -> bool {
    match node {
        FTSNode::Vector { .. } => true,
        FTSNode::Term { .. } | FTSNode::Phrase { .. } => false,
        FTSNode::And(l, r) | FTSNode::Or(l, r) => has_vector_signal(l) || has_vector_signal(r),
        FTSNode::Not(inner) => has_vector_signal(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whitespace_tokenizer(_field: Option<&str>, phrase: &str) -> Vec<String> {
        phrase
            .split_whitespace()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    }

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
    fn compile_phrase_intersects_terms() {
        let ast = FTSNode::Phrase {
            field: Some("body".into()),
            phrase: "rust ferris crab".into(),
        };
        let op = compile(&ast, None, &whitespace_tokenizer);
        match op {
            OperatorTree::Intersect(children) => assert_eq!(children.len(), 3),
            _ => panic!("expected Intersect"),
        }
    }

    #[test]
    fn compile_mixed_and_uses_robust_positive_evidence_pool() {
        let ast = FTSNode::And(
            Box::new(FTSNode::Term {
                field: None,
                term: "rust".into(),
            }),
            Box::new(FTSNode::Vector {
                field: None,
                values: vec![1.0, 0.0],
            }),
        );
        let op = compile(&ast, Some("body"), &whitespace_tokenizer);
        assert!(matches!(
            op,
            OperatorTree::RobustPositiveEvidencePool {
                alpha: 0.5,
                gating: GatingSpec::Softplus,
                ..
            }
        ));
    }

    #[test]
    fn compile_resolves_all_field_to_none() {
        let ast = FTSNode::Term {
            field: Some("_all".into()),
            term: "rust".into(),
        };
        let op = compile(&ast, Some("body"), &whitespace_tokenizer);
        match op {
            OperatorTree::Term { field, .. } => assert!(field.is_none()),
            _ => panic!("expected Term"),
        }
    }

    #[test]
    fn compile_leaves_text_scoring_unbound() {
        let ast = FTSNode::Term {
            field: Some("body".into()),
            term: "rust".into(),
        };
        let op = compile(&ast, None, &whitespace_tokenizer);
        match op {
            OperatorTree::Term { scoring, .. } => assert!(scoring.is_none()),
            _ => panic!("expected Term"),
        }
    }
}
