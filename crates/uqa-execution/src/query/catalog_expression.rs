//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate declared catalog and procedural expressions through physical scalar plans.

use crate::scalar::plan::{eval_physical_scalar, PhysicalEvalContext};
use crate::{
    query::{relational::QueryExpressionFactory, CteScope},
    RowSchema,
};
use std::sync::Arc;
use uqa_core::{
    memory::{OwnedProductionControl, Produced},
    Value,
};
use uqa_sql::{
    expr::RowLookup,
    plan::{ExpressionPlan, MutableExpressionPlan, OwnedScalarExpr, RetainedQueryArena},
    ColumnType, ResultRow, SQLError, SQLParam,
};

mod production;
pub use production::evaluate_catalog_scalar_with_control;

/// Keep inferred source metadata alive through the receiving assignment or routine coercion.
pub type TypedCatalogValue = (Value, Option<Produced<ColumnType>>);

/// Lower a borrowed schema or procedural expression into the physical scalar IR.
pub fn eval_lowered_expression<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    eval_lowered_expression_with_type(factory, scope, expression, row, params)
        .map(|(value, _)| value)
}

/// Preserve the declared source type independently of the runtime value carrier.
pub fn eval_lowered_expression_with_type<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<TypedCatalogValue, SQLError> {
    let resources = factory.production_resources()?;
    let expression = ExpressionPlan::lower_borrowed_for_rewrite(expression, &resources.control())?;
    let schema = RowSchema::empty_with_control(&resources.control())?;
    let prepared = PreparedCatalogExpression::prepare(
        factory, scope, expression, &schema, params, true, resources,
    )?;
    prepared.resources.control().check()?;
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(prepared.hook.as_ref())
        .with_subquery_runner(prepared.hook.as_ref());
    let value = eval_physical_scalar(&prepared.expression, &prepared.subqueries, &context)?;
    Ok((value, prepared.source))
}

/// Evaluate with the analyzed column identities and types of a borrowed row.
pub fn eval_lowered_expression_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: &dyn RowLookup,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    eval_lowered_expression_with_schema_and_type(factory, scope, expression, row, schema, params)
        .map(|(value, _)| value)
}

/// Keep the generated expression's source type through its receiving column assignment.
pub fn eval_lowered_expression_with_schema_and_type<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: &dyn RowLookup,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<TypedCatalogValue, SQLError> {
    PreparedCatalogExpression::lower(factory, scope, expression, schema, params)?
        .into_evaluated(row)
}

/// Admit an analyzed catalog expression without resolving its written routine names again.
pub fn eval_expression_plan_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &ExpressionPlan,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let resources = factory.production_resources()?;
    let expression = expression.copy_for_rewrite(&resources.control())?;
    // Planner-proven constant routines enter here: another optimization would recurse into the same evaluator.
    PreparedCatalogExpression::prepare(
        factory, scope, expression, schema, params, false, resources,
    )?
    .evaluate_value(row)
}

/// Evaluate a borrowed planned scalar without constructing an ordinary temporary expression tree.
pub fn eval_scalar_plan_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ScalarExpr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let resources = factory.production_resources()?;
    let expression = MutableExpressionPlan::copy_from(expression, &[], &resources.control())?;
    PreparedCatalogExpression::prepare(
        factory, scope, expression, schema, params, false, resources,
    )?
    .evaluate_value(row)
}

/// A typed expression bound once for a schema rewrite. Query slots and declarations share the same admitted namespace in every row evaluation.
pub(crate) struct PreparedCatalogExpression<'a> {
    expression: OwnedScalarExpr,
    subqueries: RetainedQueryArena,
    source: Option<Produced<ColumnType>>,
    schema: RowSchema,
    params: &'a [SQLParam],
    hook: Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    resources: OwnedProductionControl,
}

impl<'a> PreparedCatalogExpression<'a> {
    pub(crate) fn lower<S: Clone + 'static>(
        factory: &'a dyn QueryExpressionFactory<S>,
        scope: CteScope<S>,
        expression: &uqa_sql::ast::Expr,
        schema: &RowSchema,
        params: &'a [SQLParam],
    ) -> Result<Self, SQLError> {
        let resources = factory.production_resources()?;
        let expression =
            ExpressionPlan::lower_borrowed_for_rewrite(expression, &resources.control())?;
        Self::prepare(factory, scope, expression, schema, params, true, resources)
    }

    fn prepare<S: Clone + 'static>(
        factory: &'a dyn QueryExpressionFactory<S>,
        mut scope: CteScope<S>,
        expression: MutableExpressionPlan,
        schema: &RowSchema,
        params: &'a [SQLParam],
        optimize: bool,
        resources: OwnedProductionControl,
    ) -> Result<Self, SQLError> {
        let control = resources.control();
        let (expression, subqueries) = expression.into_parts(&control)?;
        let subqueries = subqueries.into_shared(&control)?;
        scope.scalar_subqueries = subqueries.clone();
        let hook = factory.bind_scope(&scope)?;
        let source = uqa_sql::type_resolution::scalar_type_with_resolver_and_control(
            &expression,
            schema,
            params,
            Some(hook.as_ref()),
            &control,
        )?;
        let mut expression = uqa_sql::type_resolution::bind_type_introspection_owned(
            expression,
            schema,
            params,
            Some(hook.as_ref()),
            &control,
        )?;
        if optimize {
            expression = factory.optimize_owned_catalog_scalar(expression, &control)?;
        }
        Ok(Self {
            expression,
            subqueries,
            source,
            schema: schema.clone(),
            params,
            hook,
            resources,
        })
    }

    pub(crate) fn evaluate_value(&self, row: &dyn RowLookup) -> Result<Value, SQLError> {
        self.resources.control().check()?;
        let row = CatalogRowView {
            schema: &self.schema,
            row,
        };
        let context = PhysicalEvalContext::from_row_lookup(&row, self.params)
            .with_row_schema(&self.schema)
            .with_function_hook(self.hook.as_ref())
            .with_subquery_runner(self.hook.as_ref());
        eval_physical_scalar(&self.expression, &self.subqueries, &context)
    }

    pub(crate) fn evaluate(&self, row: &dyn RowLookup) -> Result<TypedCatalogValue, SQLError> {
        let value = self.evaluate_value(row)?;
        let source = self
            .source
            .as_ref()
            .map(|ty| ty.clone_with_control(&self.resources.control()))
            .transpose()?;
        Ok((value, source))
    }

    fn into_evaluated(self, row: &dyn RowLookup) -> Result<TypedCatalogValue, SQLError> {
        let value = self.evaluate_value(row)?;
        Ok((value, self.source))
    }
}

/// Borrow named document fields through the analyzed schema's identities, without copying a row or inventing qualified map keys.
struct CatalogRowView<'a> {
    schema: &'a RowSchema,
    row: &'a dyn RowLookup,
}

impl CatalogRowView<'_> {
    fn physical_column(&self, slot: usize) -> Option<&Value> {
        let logical = if self.schema.is_identity_layout() {
            slot
        } else {
            self.schema
                .layout_slots()
                .iter()
                .position(|value| *value == slot)?
        };
        self.positional_column(logical)
    }
}

impl RowLookup for CatalogRowView<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .column_slot(name)
            .and_then(|slot| self.physical_column(slot))
            .or_else(|| {
                self.schema
                    .has_unqualified_column(name)
                    .then(|| self.row.column(name))
                    .flatten()
            })
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column)
            .and_then(|slot| self.physical_column(slot))
            .or_else(|| {
                self.schema
                    .has_qualified_column(qualifier, column)
                    .then(|| self.row.qualified_column(qualifier, column))
                    .flatten()
            })
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.schema.qualified_column_is_ambiguous(qualifier, column)
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.schema
            .columns()
            .get(index)
            .and_then(|name| self.row.column(name))
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (index, name) in self.schema.columns().iter().enumerate() {
            visitor(name, self.positional_column(index).unwrap_or(&Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct PreparationCounter {
        binds: std::sync::atomic::AtomicUsize,
        optimizations: std::sync::atomic::AtomicUsize,
    }

    impl QueryExpressionFactory<()> for PreparationCounter {
        fn production_resources(
            &self,
        ) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
            Ok(uqa_core::memory::ProductionControl::uncontrolled().capture())
        }
        fn bind_scope(
            &self,
            scope: &CteScope,
        ) -> Result<Arc<dyn crate::query::set_projection::SetFunctionRuntime + '_>, SQLError>
        {
            assert!(scope.scalar_subqueries.is_empty());
            self.binds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Arc::new(
                crate::query::relational::values::test_support::Services,
            ))
        }
        fn optimize_owned_catalog_scalar(
            &self,
            expression: uqa_sql::plan::OwnedScalarExpr,
            _control: &uqa_core::memory::ProductionControl<'_>,
        ) -> Result<uqa_sql::plan::OwnedScalarExpr, SQLError> {
            self.optimizations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(expression)
        }

        fn optimize_catalog_scalar(&self, _: &mut crate::ScalarExpr) -> Result<(), SQLError> {
            self.optimizations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn prepare_predicate<'a>(
            &'a self,
            _: &crate::ScalarExpr,
            _: &'a [SQLParam],
            _: &CteScope,
        ) -> Result<Option<crate::SharedRowPredicate<'a>>, SQLError> {
            unreachable!("schema rewrite has no query predicate")
        }
    }

    #[test]
    fn prepared_catalog_expression_binds_once_and_preserves_each_rows_source_type() {
        let factory = PreparationCounter::default();
        let schema = RowSchema::with_types(
            vec!["source".into()],
            vec![Some(uqa_sql::ColumnType::SmallInteger)],
        );
        let expression = PreparedCatalogExpression::lower(
            &factory,
            CteScope::default(),
            &uqa_sql::ast::Expr::Column("source".into()),
            &schema,
            &[],
        )
        .unwrap();
        for input in 0..100 {
            let row = ResultRow::from([("source".into(), Value::Int(input))]);
            let (value, source) = expression.evaluate(&row).unwrap();
            assert_eq!(value, Value::Int(input));
            assert_eq!(source.as_deref(), Some(&uqa_sql::ColumnType::SmallInteger));
        }
        assert_eq!(factory.binds.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            factory
                .optimizations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn prepared_catalog_expression_preserves_lazy_row_errors() {
        let factory = PreparationCounter::default();
        let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(source int, g int GENERATED ALWAYS AS (CASE WHEN source>0 THEN source ELSE 100/source END) STORED)").unwrap().remove(0) else { panic!("table"); };
        let schema = RowSchema::with_types(
            vec!["source".into()],
            vec![Some(uqa_sql::ColumnType::Integer)],
        );
        let expression = PreparedCatalogExpression::lower(
            &factory,
            CteScope::default(),
            &table.columns[1].generated.as_ref().unwrap().expression,
            &schema,
            &[],
        )
        .unwrap();
        for (input, expected) in [(1, 1), (-2, -50), (3, 3)] {
            let row = ResultRow::from([("source".into(), Value::Int(input))]);
            assert_eq!(expression.evaluate(&row).unwrap().0, Value::Int(expected));
        }
        let row = ResultRow::from([("source".into(), Value::Int(0))]);
        assert!(expression
            .evaluate(&row)
            .unwrap_err()
            .to_string()
            .contains("division by zero"));
        assert_eq!(
            factory
                .optimizations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn virtual_attributes_use_the_borrowed_row_only_inside_the_declared_namespace() {
        struct Row(Value);
        impl RowLookup for Row {
            fn column(&self, name: &str) -> Option<&Value> {
                (name == "tableoid").then_some(&self.0)
            }
            fn qualified_column(&self, qualifier: &str, name: &str) -> Option<&Value> {
                (qualifier == "scoped").then(|| self.column(name)).flatten()
            }
        }
        let schema = RowSchema::with_typed_virtual_identities(
            &RowSchema::default(),
            &[
                (
                    uqa_sql::ColumnIdentity::unqualified("tableoid"),
                    Some(uqa_sql::ColumnType::Oid),
                ),
                (
                    uqa_sql::ColumnIdentity::qualified("scoped", "tableoid"),
                    Some(uqa_sql::ColumnType::Oid),
                ),
            ],
        );
        let row = Row(Value::Int(80001));
        let view = CatalogRowView {
            schema: &schema,
            row: &row,
        };
        assert!(std::ptr::eq(
            view.column("tableoid").unwrap(),
            std::ptr::from_ref(&row.0)
        ));
        assert!(std::ptr::eq(
            view.qualified_column("scoped", "tableoid").unwrap(),
            std::ptr::from_ref(&row.0)
        ));
        assert!(view.qualified_column("other", "tableoid").is_none());
        let mut visited = 0;
        view.visit_columns(&mut |_, _| visited += 1);
        assert_eq!(visited, 0);
        let hidden = CatalogRowView {
            schema: &RowSchema::default(),
            row: &row,
        };
        assert!(hidden.column("tableoid").is_none());
    }

    #[test]
    fn analyzed_qualification_reads_the_original_named_field_without_copying_it() {
        let schema = RowSchema::with_identity_aliases(
            &RowSchema::new(vec!["a.b".into()]),
            &[(uqa_sql::ColumnIdentity::qualified("table.one", "a.b"), 0)],
        );
        let row = ResultRow::from([("a.b".into(), Value::Int(13))]);
        let view = CatalogRowView {
            schema: &schema,
            row: &row,
        };
        assert!(std::ptr::eq(
            view.qualified_column("table.one", "a.b").unwrap(),
            std::ptr::from_ref(&row["a.b"])
        ));
        assert!(view.qualified_column("table", "one.a.b").is_none());
        let expression = crate::ScalarExpr::QualifiedColumn {
            qualifier: "table.one".into(),
            column: "a.b".into(),
        };
        assert_eq!(
            crate::scalar::plan::eval_physical_scalar(
                &expression,
                &[],
                &PhysicalEvalContext::from_row_lookup(&view, &[]).with_row_schema(&schema)
            )
            .unwrap(),
            Value::Int(13)
        );

        let projected = RowSchema::select(
            &RowSchema::new(vec!["unused".into(), "a.b".into()]),
            &[("a.b".into(), "a.b".into())],
        );
        let projected = RowSchema::with_identity_aliases(
            &projected,
            &[(uqa_sql::ColumnIdentity::qualified("table.one", "a.b"), 0)],
        );
        let view = CatalogRowView {
            schema: &projected,
            row: &row,
        };
        assert_eq!(view.column("a.b"), Some(&Value::Int(13)));
        assert!(std::ptr::eq(
            view.qualified_column("table.one", "a.b").unwrap(),
            std::ptr::from_ref(&row["a.b"])
        ));
    }
}
