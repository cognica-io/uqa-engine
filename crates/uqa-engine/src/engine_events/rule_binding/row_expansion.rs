//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time expansion of rewrite-rule OLD/NEW row stars.

use std::collections::BTreeSet;

use uqa_sql::ast::{ColumnType, Expr, RuleEvent, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

pub(crate) fn expand_rule_action_row_stars(
    action: &Statement,
    action_columns: &BTreeSet<String>,
    event_columns: &[(String, ColumnType)],
    event: RuleEvent,
) -> Result<Statement, SQLError> {
    super::bind_rule_action(
        action,
        action_columns,
        &mut RuleRowStarExpander {
            columns: event_columns,
            event,
        },
    )
}

struct RuleRowStarExpander<'a> {
    columns: &'a [(String, ColumnType)],
    event: RuleEvent,
}

impl RuleRowStarExpander<'_> {
    fn invalid_reference(&self, qualifier: &str) -> Option<SQLError> {
        let invalid = qualifier.eq_ignore_ascii_case("old")
            && matches!(self.event, RuleEvent::Select | RuleEvent::Insert)
            || qualifier.eq_ignore_ascii_case("new")
                && matches!(self.event, RuleEvent::Select | RuleEvent::Delete);
        invalid.then(|| SQLError::Routine {
            sqlstate: "42P17".into(),
            message: format!(
                "ON {} rule cannot use {}",
                rule_event_name(self.event),
                qualifier.to_ascii_uppercase()
            ),
        })
    }
}

impl VariableResolver for RuleRowStarExpander<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>, SQLError> {
        if !qualifier.eq_ignore_ascii_case("old") && !qualifier.eq_ignore_ascii_case("new") {
            return Ok(None);
        }
        if let Some(error) = self.invalid_reference(qualifier) {
            return Err(error);
        }
        Ok(Some(
            self.columns
                .iter()
                .map(|(column, _)| Expr::qualified_column(qualifier, column))
                .collect(),
        ))
    }
}

const fn rule_event_name(event: RuleEvent) -> &'static str {
    match event {
        RuleEvent::Select => "SELECT",
        RuleEvent::Insert => "INSERT",
        RuleEvent::Update => "UPDATE",
        RuleEvent::Delete => "DELETE",
    }
}
