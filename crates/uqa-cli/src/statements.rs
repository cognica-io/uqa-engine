//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL statement boundary detection and splitting.

use pg_query::protobuf::Token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatementHead {
    Start,
    Create,
    CreateOr,
    CreateOrReplace,
    RoutineDeclaration,
    Other,
}

impl StatementHead {
    fn observe(self, token: Token) -> Self {
        match (self, token) {
            (Self::Start, Token::Create) => Self::Create,
            (Self::Create, Token::Or) => Self::CreateOr,
            (Self::CreateOr, Token::Replace) => Self::CreateOrReplace,
            (Self::Create | Self::CreateOrReplace, Token::Function | Token::Procedure) => {
                Self::RoutineDeclaration
            }
            (Self::RoutineDeclaration, _) => Self::RoutineDeclaration,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndDelimitedConstruct {
    AtomicBody,
    CaseExpression,
}

#[derive(Debug)]
struct StatementBoundaryScanner {
    offsets: Vec<usize>,
    parenthesis_depth: usize,
    end_delimited: Vec<EndDelimitedConstruct>,
    statement_head: StatementHead,
    previous_token: Option<Token>,
}

impl StatementBoundaryScanner {
    fn new() -> Self {
        Self {
            offsets: Vec::new(),
            parenthesis_depth: 0,
            end_delimited: Vec::new(),
            statement_head: StatementHead::Start,
            previous_token: None,
        }
    }

    fn observe(&mut self, token: Token, offset: usize) {
        if matches!(token, Token::SqlComment | Token::CComment) {
            return;
        }
        if self.starts_atomic_body(token) {
            self.end_delimited.push(EndDelimitedConstruct::AtomicBody);
            self.statement_head = StatementHead::Start;
            self.previous_token = Some(token);
            return;
        }
        if !self.end_delimited.is_empty() && token == Token::Case {
            self.end_delimited
                .push(EndDelimitedConstruct::CaseExpression);
        } else if !self.end_delimited.is_empty()
            && token == Token::EndP
            && self.end_delimited.pop() == Some(EndDelimitedConstruct::AtomicBody)
        {
            self.statement_head = StatementHead::Other;
        }

        match token {
            Token::Ascii40 => self.parenthesis_depth += 1,
            Token::Ascii41 => {
                self.parenthesis_depth = self.parenthesis_depth.saturating_sub(1);
            }
            Token::Ascii59 if self.parenthesis_depth == 0 => {
                self.observe_semicolon(offset);
                return;
            }
            _ => {}
        }
        self.statement_head = self.statement_head.observe(token);
        self.previous_token = Some(token);
    }

    fn starts_atomic_body(&self, token: Token) -> bool {
        token == Token::Atomic
            && self.previous_token == Some(Token::BeginP)
            && self.statement_head == StatementHead::RoutineDeclaration
            && self.parenthesis_depth == 0
    }

    fn observe_semicolon(&mut self, offset: usize) {
        if self.end_delimited.is_empty() {
            self.offsets.push(offset);
        }
        self.statement_head = StatementHead::Start;
        self.previous_token = None;
    }
}

/// Byte offsets of statement-terminating semicolons according to the `PostgreSQL` 18 lexer, extended with the grammar-level nesting of SQL-standard `BEGIN ATOMIC ... END` routine bodies.
pub(super) fn statement_terminator_offsets(text: &str) -> Vec<usize> {
    let Ok(scanned) = pg_query::scan(text) else {
        // A lexical error (for example, an unterminated quote) consumes the remaining input. Keeping it as one statement prevents an apparent semicolon inside that token from executing a truncated prefix.
        return Vec::new();
    };
    let mut boundaries = StatementBoundaryScanner::new();
    for scanned_token in scanned.tokens {
        let Ok(token) = Token::try_from(scanned_token.token) else {
            continue;
        };
        let Ok(offset) = usize::try_from(scanned_token.start) else {
            continue;
        };
        boundaries.observe(token, offset);
    }
    boundaries.offsets
}

pub(super) fn split_statements(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut start = 0;
    for offset in statement_terminator_offsets(text) {
        let statement = text[start..offset].trim();
        if !statement.is_empty() {
            out.push(statement.to_string());
        }
        start = offset + 1;
    }
    let trailing = text[start..].trim();
    if !trailing.is_empty() {
        out.push(trailing.to_string());
    }
    out
}

pub(super) fn contains_statement_terminator(text: &str) -> bool {
    !statement_terminator_offsets(text).is_empty()
}

pub(super) fn statement_is_pure_comment(statement: &str) -> bool {
    statement
        .lines()
        .all(|line| line.trim().is_empty() || line.trim().starts_with("--"))
}
