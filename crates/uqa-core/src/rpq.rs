//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular path expression syntax shared by planning and graph execution.
//!
//! Alternation has lower precedence than concatenation; repetition binds tightest.
//! This module contains no graph store, traversal, or automaton implementation.

/// Regular path expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RegularPathExpr {
    /// A single edge label.
    Label(String),
    /// `lhs / rhs`.
    Concat(Box<RegularPathExpr>, Box<RegularPathExpr>),
    /// `lhs | rhs`.
    Alternation(Box<RegularPathExpr>, Box<RegularPathExpr>),
    /// `inner *`.
    KleeneStar(Box<RegularPathExpr>),
    /// `inner { min, max }`.
    Bounded {
        inner: Box<RegularPathExpr>,
        min: u32,
        max: u32,
    },
}

impl RegularPathExpr {
    pub fn label(name: impl Into<String>) -> Self {
        Self::Label(name.into())
    }
    pub fn concat(left: Self, right: Self) -> Self {
        Self::Concat(Box::new(left), Box::new(right))
    }
    pub fn alt(left: Self, right: Self) -> Self {
        Self::Alternation(Box::new(left), Box::new(right))
    }
    pub fn star(inner: Self) -> Self {
        Self::KleeneStar(Box::new(inner))
    }
    pub fn bounded(inner: Self, min: u32, max: u32) -> Self {
        Self::Bounded {
            inner: Box::new(inner),
            min,
            max,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RPQParseError {
    #[error("unexpected token at position {position}: {token:?}")]
    Unexpected { position: usize, token: String },
    #[error("unexpected end of expression")]
    Eof,
    #[error("missing closing parenthesis")]
    MissingParen,
    #[error("malformed bounded repetition: {0}")]
    MalformedBound(String),
}

pub fn parse_rpq(expr: &str) -> Result<RegularPathExpr, RPQParseError> {
    let tokens = tokenize(expr);
    let (result, pos) = parse_alternation(&tokens, 0)?;
    if pos != tokens.len() {
        return Err(RPQParseError::Unexpected {
            position: pos,
            token: tokens[pos].clone(),
        });
    }
    Ok(result)
}

fn tokenize(expr: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if matches!(ch, '(' | ')' | '/' | '|' | '*' | '{' | '}' | ',') {
            tokens.push(ch.to_string());
            i += 1;
        } else {
            let start = i;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_ascii_whitespace()
                    || matches!(c, '(' | ')' | '/' | '|' | '*' | '{' | '}' | ',')
                {
                    break;
                }
                i += 1;
            }
            tokens.push(expr[start..i].to_string());
        }
    }
    tokens
}

fn parse_alternation(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut left, p) = parse_concat(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && tokens[pos] == "|" {
        pos += 1;
        let (right, p) = parse_concat(tokens, pos)?;
        pos = p;
        left = RegularPathExpr::alt(left, right);
    }
    Ok((left, pos))
}

fn parse_concat(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut left, p) = parse_star(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && tokens[pos] == "/" {
        pos += 1;
        let (right, p) = parse_star(tokens, pos)?;
        pos = p;
        left = RegularPathExpr::concat(left, right);
    }
    Ok((left, pos))
}

fn parse_star(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let (mut expr, p) = parse_atom(tokens, pos)?;
    pos = p;
    while pos < tokens.len() && (tokens[pos] == "*" || tokens[pos] == "{") {
        if tokens[pos] == "*" {
            pos += 1;
            expr = RegularPathExpr::star(expr);
        } else {
            pos += 1;
            let min = tokens
                .get(pos)
                .ok_or_else(|| RPQParseError::MalformedBound("missing min".into()))?
                .parse::<u32>()
                .map_err(|e| RPQParseError::MalformedBound(format!("min: {e}")))?;
            pos += 1;
            if tokens.get(pos).map(String::as_str) != Some(",") {
                return Err(RPQParseError::MalformedBound("expected ','".into()));
            }
            pos += 1;
            let max = tokens
                .get(pos)
                .ok_or_else(|| RPQParseError::MalformedBound("missing max".into()))?
                .parse::<u32>()
                .map_err(|e| RPQParseError::MalformedBound(format!("max: {e}")))?;
            if min > max {
                return Err(RPQParseError::MalformedBound(format!(
                    "min {min} exceeds max {max}"
                )));
            }
            pos += 1;
            if tokens.get(pos).map(String::as_str) != Some("}") {
                return Err(RPQParseError::MalformedBound("expected '}'".into()));
            }
            pos += 1;
            expr = RegularPathExpr::bounded(expr, min, max);
        }
    }
    Ok((expr, pos))
}

fn parse_atom(
    tokens: &[String],
    mut pos: usize,
) -> Result<(RegularPathExpr, usize), RPQParseError> {
    let token = tokens.get(pos).ok_or(RPQParseError::Eof)?;
    if token == "(" {
        pos += 1;
        let (inner, p) = parse_alternation(tokens, pos)?;
        pos = p;
        if tokens.get(pos).map(String::as_str) != Some(")") {
            return Err(RPQParseError::MissingParen);
        }
        pos += 1;
        Ok((inner, pos))
    } else if matches!(token.as_str(), ")" | "/" | "|" | "*" | "{" | "}" | ",") {
        Err(RPQParseError::Unexpected {
            position: pos,
            token: token.clone(),
        })
    } else {
        pos += 1;
        Ok((RegularPathExpr::label(token.clone()), pos))
    }
}

#[cfg(test)]
mod tests;
