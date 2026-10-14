//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate declared catalog and procedural expressions through physical scalar plans.

use crate::scalar::plan::{eval_physical, PhysicalEvalContext};
use crate::{
    query::{relational::QueryExpressionFactory, CteScope},
    PhysicalRow, RowSchema, RowSchemaExecution,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::{plan::ExpressionPlan, ResultRow, SQLError, SQLParam};

/// Lower an AST expression that belongs to a schema or procedural boundary,
/// then execute the resulting physical scalar IR. Runtime consumers never
/// invoke the AST evaluator or dispatch an AST subquery directly.
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

/// Evaluate a standalone expression while retaining its declared SQL type.
/// Procedural statements such as `FOREACH` need the type because domains and
/// true arrays can share the same runtime value carrier.
pub fn eval_lowered_expression_with_type<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<(Value, Option<uqa_sql::ast::ColumnType>), SQLError> {
    let mut expression = ExpressionPlan::lower(expression.clone());
    scope.scalar_subqueries.clone_from(&expression.subqueries);
    let hook = factory.bind_scope(scope);
    let declared_type = crate::scalar_type_with_resolver(
        &expression.scalar,
        &RowSchema::default(),
        params,
        hook.as_ref(),
    )?;
    expression.scalar = crate::bind_type_introspection_with_resolver(
        expression.scalar,
        &RowSchema::default(),
        params,
        hook.as_ref(),
    );
    factory.optimize_catalog_scalar(&mut expression.scalar)?;
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    let value = eval_physical(&expression, &context)?;
    Ok((value, declared_type))
}

/// Evaluate a catalog expression against a row while preserving the declared
/// SQL types of its columns. Values alone cannot distinguish, for example,
/// `smallint` from `integer`, so schema-owned expressions must bind before
/// they cross into the physical evaluator.
pub fn eval_lowered_expression_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    eval_lowered_expression_with_schema_and_type(factory, scope, expression, row, schema, params)
        .map(|(value, _)| value)
}

/// Keep the analyzed source type with a schema expression's value for assignment into a generated column.
pub fn eval_lowered_expression_with_schema_and_type<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
    eval_catalog_expression_with_schema(
        factory,
        scope,
        ExpressionPlan::lower(expression.clone()),
        row,
        schema,
        params,
        true,
    )
}

/// Evaluate an analyzed catalog expression with its original column types, without resolving its written routine names again.
pub fn eval_expression_plan_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: ExpressionPlan,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    // Planner-proven constant routines enter here: planning again would recurse into the same constant evaluator.
    eval_catalog_expression_with_schema(factory, scope, expression, row, schema, params, false)
        .map(|(value, _)| value)
}

/// A typed expression bound to one schema-rewrite command. Its retained scope and parameters must remain the same for every row.
pub(crate) struct PreparedCatalogExpression<'a> {
    expression: ExpressionPlan,
    source: Option<uqa_sql::ColumnType>,
    schema: RowSchema,
    params: &'a [SQLParam],
    hook: Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
}

impl<'a> PreparedCatalogExpression<'a> {
    pub(crate) fn lower<S: Clone + 'static>(
        factory: &'a dyn QueryExpressionFactory<S>,
        scope: CteScope<S>,
        expression: &uqa_sql::ast::Expr,
        schema: &RowSchema,
        params: &'a [SQLParam],
    ) -> Result<Self, SQLError> {
        Self::prepare(
            factory,
            scope,
            ExpressionPlan::lower(expression.clone()),
            schema,
            params,
            true,
        )
    }

    fn prepare<S: Clone + 'static>(
        factory: &'a dyn QueryExpressionFactory<S>,
        mut scope: CteScope<S>,
        mut expression: ExpressionPlan,
        schema: &RowSchema,
        params: &'a [SQLParam],
        optimize: bool,
    ) -> Result<Self, SQLError> {
        scope.scalar_subqueries.clone_from(&expression.subqueries);
        let hook = factory.bind_scope(scope);
        let source =
            crate::scalar_type_with_resolver(&expression.scalar, schema, params, hook.as_ref())?;
        expression.scalar = crate::bind_type_introspection_with_resolver(
            expression.scalar,
            schema,
            params,
            hook.as_ref(),
        );
        if optimize {
            factory.optimize_catalog_scalar(&mut expression.scalar)?;
        }
        Ok(Self {
            expression,
            source,
            schema: schema.clone(),
            params,
            hook,
        })
    }

    pub(crate) fn evaluate(
        &self,
        row: &ResultRow,
    ) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
        let row = CatalogRowView {
            schema: &self.schema,
            row,
        };
        let context = PhysicalEvalContext::from_row_lookup(&row, self.params)
            .with_row_schema(&self.schema)
            .with_function_hook(self.hook.as_ref())
            .with_subquery_runner(self.hook.as_ref());
        eval_physical(&self.expression, &context).map(|value| (value, self.source.clone()))
    }
}

fn eval_catalog_expression_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: ExpressionPlan,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
    optimize: bool,
) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
    PreparedCatalogExpression::prepare(factory, scope, expression, schema, params, optimize)?
        .evaluate(row)
}

/// Borrow named document fields through the analyzed schema's identities, without copying a row or inventing qualified map keys.
struct CatalogRowView<'a> {
    schema: &'a RowSchema,
    row: &'a ResultRow,
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
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column)
            .and_then(|slot| self.physical_column(slot))
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
            .and_then(|name| self.row.get(name))
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (index, name) in self.schema.columns().iter().enumerate() {
            visitor(name, self.positional_column(index).unwrap_or(&Value::Null));
        }
    }
}

/// Execute a catalog-bound scalar plan against one typed physical row while applying an independent relation-privilege subject to every nested query.
pub fn eval_stored_expression_plan_with_row<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    expression: &ExpressionPlan,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let mut expression = expression.clone();
    scope.scalar_subqueries.clone_from(&expression.subqueries);
    let hook = factory.bind_scope(scope);
    crate::scalar_type_with_resolver(&expression.scalar, schema, params, hook.as_ref())?;
    expression.scalar = crate::bind_type_introspection_with_resolver(
        expression.scalar,
        schema,
        params,
        hook.as_ref(),
    );
    let view = schema.view(row);
    let context = PhysicalEvalContext::from_row_lookup(&view, params)
        .with_row_schema(schema)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref())
        .with_physical_outer_row(schema, row);
    eval_physical(&expression, &context)
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
        fn bind_scope(
            &self,
            scope: CteScope,
        ) -> Arc<dyn crate::query::set_projection::SetFunctionRuntime + '_> {
            assert!(scope.scalar_subqueries.is_empty());
            self.binds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Arc::new(crate::query::relational::values::test_support::Services)
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
            assert_eq!(
                expression.evaluate(&row).unwrap(),
                (Value::Int(input), Some(uqa_sql::ColumnType::SmallInteger))
            );
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
