//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scope-aware checks for rule-action target aliases that collide with OLD or NEW.

use crate::ast::{Expr, Statement};
use crate::plpgsql::{ResolvedVariable, VariableResolver};
use crate::SQLError;

pub(super) fn event_field_reference<'a>(
    expression: &'a Expr,
    scope: &super::RuleBindingScope,
) -> Option<(&'a str, &'a str)> {
    let Expr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return None;
    };
    if binding.dispatch != Some(crate::ast::FunctionDispatch::FieldSelect) {
        return None;
    }
    let [row, Expr::Literal(uqa_core::Value::Str(column))] = args.as_slice() else {
        return None;
    };
    let (Expr::Column(qualifier) | Expr::QualifiedStar(qualifier)) = row else {
        return None;
    };
    if !matches!(qualifier.as_str(), "old" | "new")
        || scope.qualifier_is_shadowed(qualifier)
        || matches!(row, Expr::Column(_)) && scope.column_is_shadowed(qualifier)
    {
        return None;
    }
    Some((qualifier, column))
}

pub(super) fn action_target_qualifier_referenced(
    catalog: &dyn super::RuleSourceCatalog,
    action: &Statement,
    qualifier: &str,
) -> bool {
    let context = super::RuleBindingContext::with_catalog(catalog, false);
    match action {
        Statement::Update(update) => {
            if !update.returning.is_empty() {
                return true;
            }
            let mut detector = QualifierReferenceDetector::new(qualifier);
            let mut scope = super::RuleBindingScope::default();
            if let Some(source) = &update.from {
                let _ = super::inspection::source(source, &mut detector, &scope, &context);
                let _ = super::collect_visible_scope(source, &context, &mut scope);
            }
            for expression in update
                .assignments
                .iter()
                .flat_map(|(target, value)| target.expressions().chain(std::iter::once(value)))
            {
                let _ = super::inspection::scoped_expression(
                    expression,
                    &mut detector,
                    &scope,
                    &context,
                );
            }
            if let Some(expression) = &update.r#where {
                let _ = super::inspection::scoped_expression(
                    expression,
                    &mut detector,
                    &scope,
                    &context,
                );
            }
            detector.referenced
        }
        Statement::Delete(delete) => {
            if !delete.returning.is_empty() {
                return true;
            }
            let mut detector = QualifierReferenceDetector::new(qualifier);
            let mut scope = super::RuleBindingScope::default();
            if let Some(source) = &delete.using {
                let _ = super::inspection::source(source, &mut detector, &scope, &context);
                let _ = super::collect_visible_scope(source, &context, &mut scope);
            }
            if let Some(expression) = &delete.r#where {
                let _ = super::inspection::scoped_expression(
                    expression,
                    &mut detector,
                    &scope,
                    &context,
                );
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

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.referenced |= qualifier.eq_ignore_ascii_case(self.qualifier);
        Ok(None)
    }
}
