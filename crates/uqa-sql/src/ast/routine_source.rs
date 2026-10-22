//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition-time source mapping, following `PostgreSQL`'s `pg_proc.c` literal matching.

/// Transient mapping from a routine's decoded source to its original SQL message. Ambiguous matches deliberately have no mapping, as in `PostgreSQL`'s `match_prosrc_to_query`.
#[derive(Debug, Clone)]
pub struct RoutineBodyOrigin {
    prefix_characters: u32,
    escaped_characters: Vec<u32>,
}

impl RoutineBodyOrigin {
    pub(crate) fn capture(body: &str, message: &str) -> Option<Self> {
        let mut matched = None;
        for (character_index, (offset, delimiter)) in message.char_indices().enumerate() {
            let literal = &message[offset + delimiter.len_utf8()..];
            let escapes = match delimiter {
                '$' if literal
                    .strip_prefix(body)
                    .is_some_and(|rest| rest.starts_with('$')) =>
                {
                    Vec::new()
                }
                '\'' => match match_literal(body, literal) {
                    Some(escapes) => escapes,
                    None => continue,
                },
                _ => continue,
            };
            if matched.is_some() {
                return None;
            }
            matched = Some(Self {
                prefix_characters: u32::try_from(character_index + 1).ok()?,
                escaped_characters: escapes,
            });
        }
        matched
    }

    pub(crate) fn position(&self, position: u32) -> Option<u32> {
        let before = position.checked_sub(1)?;
        let escapes = self
            .escaped_characters
            .partition_point(|index| *index < before);
        self.prefix_characters
            .checked_add(position)?
            .checked_add(u32::try_from(escapes).ok()?)
    }
}

fn match_literal(body: &str, literal: &str) -> Option<Vec<u32>> {
    let mut literal = literal.chars().peekable();
    let mut escaped = Vec::new();
    for (index, character) in body.chars().enumerate() {
        match literal.peek().copied()? {
            '\\' => {
                literal.next();
                escaped.push(u32::try_from(index).ok()?);
            }
            '\'' => {
                literal.next();
                if literal.peek() != Some(&'\'') {
                    return None;
                }
                escaped.push(u32::try_from(index).ok()?);
            }
            _ => {}
        }
        if literal.next() != Some(character) {
            return None;
        }
    }
    (literal.next() == Some('\'') && literal.next() != Some('\'')).then_some(escaped)
}

impl super::CreateFunction {
    /// Transpose a SQL string body's cursor into its defining request, or retain its internal source when no defining request is available. Other body forms retain their existing diagnostic.
    pub fn source_body_error(&self, error: crate::SQLError) -> crate::SQLError {
        match &self.body {
            super::FunctionBody::Source(body) if self.language == "sql" => {
                error.in_routine_source(body, self.body_origin.as_ref())
            }
            _ => error,
        }
    }
}
