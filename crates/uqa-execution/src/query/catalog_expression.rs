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
    row: &dyn RowLookup,
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
    row: &dyn RowLookup,
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

fn eval_catalog_expression_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    mut expression: ExpressionPlan,
    row: &dyn RowLookup,
    schema: &RowSchema,
    params: &[SQLParam],
    optimize: bool,
) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
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
    let row = CatalogRowView { schema, row };
    let context = PhysicalEvalContext::from_row_lookup(&row, params)
        .with_row_schema(schema)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    eval_physical(&expression, &context).map(|value| (value, source))
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
