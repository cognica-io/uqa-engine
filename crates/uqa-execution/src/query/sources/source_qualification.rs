//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source filtering and scoped child-query cache restoration.

use super::{
    physical_exec_error, qualifier_filter, CteScope, JoinKind, PhysicalOperator, QualifierFilters,
    QueryOutput, QueryPlan, RowSchema, SQLError, SQLParam, ScalarExpr, SourceContext,
};

pub(super) fn execute_view_plan_output_with_parent_cache<S: Clone + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    local_cte_names: &uqa_sql::binding::production::BindingNameSet,
) -> Result<QueryOutput, SQLError> {
    let resources = context.ctes.production_resources()?;
    let mut scope = ctes.enter_cte_bindings_with_control(
        local_cte_names.iter().map(String::as_str),
        &resources.control(),
    )?;
    let ctes = &mut *scope;
    if let Some(outer) = ctes.row_lock_outer_row() {
        context
            .ctes
            .queries
            .execute_lateral_query(plan, outer, params, ctes)
    } else {
        context.ctes.queries.execute_query(plan, params, ctes)
    }
}

pub(super) fn attach_qualifier_filter<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    context: &SourceContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    crate::physical::construction::OperatorBuild::build(operator, |operator| {
        if let Some(predicate) = qualifier_filter(filters, qualifier) {
            let evaluator = context.relational.evaluator(params, ctes)?;
            operator
                .wrap(|input| Box::new(crate::Filter::with_evaluator(input, predicate, evaluator)));
        }
        Ok(())
    })
}

/// Attach the predicates placed on a source whose row carries every column of its qualifier; see [`filter_closed_source`].
pub(super) fn attach_closed_qualifier_filter<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    context: &SourceContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    filter_closed_source(operator, qualifier, filters, || {
        context.relational.evaluator(params, ctes)
    })
}

/// A placed predicate that reads an unproduced column retains its deferred diagnostic; valid predicates capture their evaluator before the parent takes ownership.
pub(super) fn filter_closed_source<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    evaluator: impl FnOnce() -> Result<crate::SharedExpressionEvaluator<'a>, SQLError>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    crate::physical::construction::OperatorBuild::build(operator, |operator| {
        let Some(predicate) = qualifier_filter(filters, qualifier) else {
            return Ok(());
        };
        if let Some(column) = unproduced_column(&predicate, qualifier, operator.row_schema()) {
            operator.wrap(|input| Box::new(crate::Filter::unresolved(input, column)));
        } else {
            let evaluator = evaluator()?;
            operator
                .wrap(|input| Box::new(crate::Filter::with_evaluator(input, predicate, evaluator)));
        }
        Ok(())
    })
}

/// The first `qualifier.column` reference of `predicate` that `schema` does not produce.
pub(super) fn unproduced_column(
    predicate: &ScalarExpr,
    qualifier: &str,
    schema: &RowSchema,
) -> Option<String> {
    let mut unproduced = None;
    predicate.visit(&mut |node| {
        if let ScalarExpr::QualifiedColumn {
            qualifier: reference,
            column,
        } = node
        {
            if unproduced.is_none()
                && reference == qualifier
                && schema.qualified_position(reference, column).is_none()
                && schema.qualified_slot(reference, column).is_none()
                && !schema.qualified_column_is_ambiguous(reference, column)
            {
                unproduced = Some(format!("{reference}.{column}"));
            }
        }
    });
    unproduced
}

pub(super) fn shape_join_using_output<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &uqa_sql::semantics::ResolvedJoinUsing,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let (operator, (columns, aliases)) =
        crate::physical::construction::OperatorBuild::run(operator, |_| {
            uqa_sql::semantics::join_using_layout(kind, left, right, using)
        })?;
    crate::JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(physical_exec_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_core::Value;

    #[test]
    fn rejected_filter_evaluator_closes_its_input_and_retains_cleanup_diagnostics() {
        struct Input {
            schema: RowSchema,
            closes: Arc<AtomicUsize>,
        }
        impl PhysicalOperator for Input {
            fn row_schema(&self) -> &RowSchema {
                &self.schema
            }
            fn open(&mut self) -> crate::ExecResult<()> {
                Ok(())
            }
            fn next(&mut self) -> crate::ExecResult<Option<crate::Batch>> {
                panic!("rejected constructor must not read");
            }
            fn close(&mut self) -> crate::ExecResult<()> {
                assert_eq!(self.closes.fetch_add(1, Ordering::SeqCst), 0);
                Err(SQLError::Routine {
                    sqlstate: "58030".into(),
                    message: "filter input close failed".into(),
                }
                .into())
            }
        }
        let closes = Arc::new(AtomicUsize::new(0));
        let input = Box::new(Input {
            schema: RowSchema::default(),
            closes: closes.clone(),
        });
        let filters = QualifierFilters::from([(
            "source".into(),
            vec![ScalarExpr::Literal(Value::Bool(true))],
        )]);
        let error = filter_closed_source(input, "source", Some(&filters), || {
            Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "evaluator admission failed".into(),
            })
        })
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert!(error
            .detail()
            .unwrap()
            .contains("filter input close failed"));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }
}
