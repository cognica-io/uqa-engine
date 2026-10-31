//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parsed-message destinations admit statement slots and shared position metadata before publication.

use super::{ParsedStatement, Result, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// Parse the complete message with the established parser and admit the project-owned statement vector and source-position index. Parser-owned protobuf nodes move into their original statement slots without copying; their opaque parser allocation remains with that dependency's existing lifetime.
pub fn parse_statements_with_control<'sql>(
    sql: &'sql str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ParsedStatement<'sql>>>> {
    control.check()?;
    let parsed = crate::parser::parse(sql)?;
    control.check()?;
    let mut output = ProductionVec::new(*control);
    output.reserve(parsed.protobuf.stmts.len())?;
    let positions =
        super::super::source_positions::SourcePositions::shared_with_control(sql, control)?;
    for raw in parsed.protobuf.stmts {
        control.check()?;
        let node = raw
            .stmt
            .ok_or_else(|| SQLError::Internal("parser returned an empty statement".into()))?;
        let source = statement_source(sql, raw.stmt_location, raw.stmt_len)?;
        output.push_produced(control.finish(
            ParsedStatement {
                sql: source,
                message: sql,
                positions: std::sync::Arc::clone(&positions),
                node,
                settings: crate::parser::settings(),
            },
            control.empty_reservation(),
        )?)?;
    }
    Ok(output.finish()?)
}

fn statement_source(sql: &str, location: i32, length: i32) -> Result<&str> {
    let start = usize::try_from(location)
        .map_err(|_| SQLError::Internal("parser returned a negative statement offset".into()))?;
    let end = if length == 0 {
        sql.len()
    } else {
        let length = usize::try_from(length).map_err(|_| {
            SQLError::Internal("parser returned a negative statement length".into())
        })?;
        start
            .checked_add(length)
            .ok_or_else(|| SQLError::Internal("parser statement offset overflow".into()))?
    };
    sql.get(start..end)
        .ok_or_else(|| SQLError::Internal("parser statement bounds do not match SQL text".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn admitted_statements_preserve_message_slices_and_shared_original_positions() {
        let sql = "SELECT '🙂'; SELECT t.value; SELECT 3";
        let expected = super::super::parse_statements(sql).unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let actual = parse_statements_with_control(sql, &control).unwrap();
        assert_eq!(actual.len(), 3);
        for (expected, actual) in expected.iter().zip(actual.iter()) {
            assert_eq!(actual.sql(), expected.sql());
            assert_eq!(
                serde_json::to_value(actual.compile().unwrap()).unwrap(),
                serde_json::to_value(expected.compile().unwrap()).unwrap()
            );
        }
        assert!(budget.used() > actual.reserved_bytes());
        drop(actual);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn cancelled_parse_does_not_enter_the_external_parser() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        token.cancel();
        let control = ProductionControl::new(&budget, &token, &token);
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                parse_statements_with_control("SELECT (", &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}
