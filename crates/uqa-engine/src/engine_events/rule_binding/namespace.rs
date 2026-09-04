//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scope-aware checks for rule-action target aliases that collide with OLD or NEW.

use std::collections::BTreeSet;

use uqa_sql::ast::{Expr, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

pub(super) fn action_target_qualifier_referenced(action: &Statement, qualifier: &str) -> bool {
    match action {
        Statement::Update(update) => {
            if !update.returning.is_empty() {
                return true;
            }
            let mut detector = QualifierReferenceDetector::new(qualifier);
            let mut scope = BTreeSet::new();
            if let Some(source) = &update.from {
                let _ = super::bind_from(source, &mut detector, &scope);
                super::collect_visible_qualifiers(source, &mut scope);
            }
            for (_, expression) in &update.assignments {
                let _ = super::bind_rule_expr_scoped(expression, &mut detector, &scope);
            }
            if let Some(expression) = &update.r#where {
                let _ = super::bind_rule_expr_scoped(expression, &mut detector, &scope);
            }
            detector.referenced
        }
        Statement::Delete(delete) => {
            if !delete.returning.is_empty() {
                return true;
            }
            let mut detector = QualifierReferenceDetector::new(qualifier);
            let mut scope = BTreeSet::new();
            if let Some(source) = &delete.using {
                let _ = super::bind_from(source, &mut detector, &scope);
                super::collect_visible_qualifiers(source, &mut scope);
            }
            if let Some(expression) = &delete.r#where {
                let _ = super::bind_rule_expr_scoped(expression, &mut detector, &scope);
            }
            detector.referenced
        }
        _ => false,
    }
}

struct QualifierReferenceDetector<'a> {
    qualifier: &'a str,
    referenced: bool,
}

impl<'a> QualifierReferenceDetector<'a> {
    const fn new(qualifier: &'a str) -> Self {
        Self {
            qualifier,
            referenced: false,
        }
    }
}

impl VariableResolver for QualifierReferenceDetector<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.referenced |= qualifier.eq_ignore_ascii_case(self.qualifier);
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>, SQLError> {
        self.referenced |= qualifier.eq_ignore_ascii_case(self.qualifier);
        Ok(None)
    }
}
