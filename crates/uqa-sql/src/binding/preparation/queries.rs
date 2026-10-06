//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query analysis follows `PostgreSQL`'s source, target, clause, and coercion order.

use super::super::{analysis, overlay_outer_schema, projection_columns, projection_star_columns};
use super::{
    error, ColumnType, ExpressionType, Preparation, QueryOutput, QueryPlan, RowSchema, SQLError,
    ScalarExpr,
};
use crate::plan::{ProjectionPlan, QueryBlockPlan, RelationalPlan};

impl Preparation<'_> {
    pub(super) fn query(
        &mut self,
        plan: &QueryPlan,
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.query_output(plan, outer, false)
            .map(|output| output.schema())
    }

    pub(super) fn query_output(
        &mut self,
        plan: &QueryPlan,
        outer: Option<&RowSchema>,
        preserve_unknown: bool,
    ) -> Result<QueryOutput, SQLError> {
        let mode = if plan.relations_bound {
            crate::catalog::resolution::RelationLookupMode::Bound
        } else {
            crate::catalog::resolution::RelationLookupMode::Dynamic
        };
        let lookup = self.scope.resolution.set_lookup_mode(mode);
        let result = (|| {
            let previous = self.ctes(&plan.ctes, outer)?;
            let result = self.root(&plan.root, outer, preserve_unknown);
            self.scope.restore_cte_schemas(previous);
            result
        })();
        self.scope.resolution.set_lookup_mode(lookup);
        result
    }

    pub(super) fn root(
        &mut self,
        root: &RelationalPlan,
        outer: Option<&RowSchema>,
        preserve_unknown: bool,
    ) -> Result<QueryOutput, SQLError> {
        match root {
            RelationalPlan::QueryBlock(block) => self.block(block, outer, preserve_unknown),
            RelationalPlan::Values { rows, subqueries } => self.values(rows, subqueries, outer),
            RelationalPlan::SetOp {
                kind,
                all,
                left,
                right,
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                let left = self.query_output(left, outer, true)?;
                let right = self.query_output(right, outer, true)?;
                let output = self.set_output(left, right, *kind, *all)?;
                let schema = output.schema();
                for order in order_by {
                    self.expression(&order.expr, &schema, subqueries)?;
                }
                let input = overlay_outer_schema(&RowSchema::default(), outer);
                self.slice(limit.as_deref(), offset.as_deref(), &input, subqueries)?;
                Ok(output)
            }
        }
    }

    pub(super) fn set_output(
        &mut self,
        mut left: QueryOutput,
        mut right: QueryOutput,
        kind: crate::ast::SetOpKind,
        all: bool,
    ) -> Result<QueryOutput, SQLError> {
        if left.types.len() != right.types.len() {
            return Err(error(
                "42601",
                format!(
                    "each {} query must have the same number of columns",
                    match kind {
                        crate::ast::SetOpKind::Union => "UNION",
                        crate::ast::SetOpKind::Intersect => "INTERSECT",
                        crate::ast::SetOpKind::Except => "EXCEPT",
                    }
                ),
            ));
        }
        for (left, right) in left.types.iter_mut().zip(&mut right.types) {
            let mut values = [left.clone(), right.clone()];
            let ty = self.common(
                crate::type_resolution::CommonTypeContext::set_operation(kind),
                &mut values,
            )?;
            let fields = super::super::record_fields::common_fields(
                values.iter().map(|value| value.record_fields.as_ref()),
            );
            *left = ExpressionType::resolved(ty.clone());
            left.record_fields.clone_from(&fields);
            *right = ExpressionType::resolved(ty);
            right.record_fields = fields;
        }
        super::super::type_resolution::set_operation_output_schema(
            &left.schema(),
            &right.schema(),
            kind,
            all,
        )?;
        Ok(left)
    }

    fn block(
        &mut self,
        block: &QueryBlockPlan,
        outer: Option<&RowSchema>,
        preserve_unknown: bool,
    ) -> Result<QueryOutput, SQLError> {
        let source = block
            .from
            .as_ref()
            .map(|source| self.source(source, &block.subqueries, outer))
            .transpose()?
            .unwrap_or_default();
        let source = analysis::with_query_source_columns(&source, block);
        let input = overlay_outer_schema(&source, outer);
        let mut output =
            self.projections(&block.projections, &source, &input, &block.subqueries)?;
        if let Some(predicate) = &block.r#where {
            self.require_boolean(predicate, &input, &block.subqueries, "WHERE")?;
        }
        if let Some(having) = &block.having {
            self.require_boolean(having, &input, &block.subqueries, "HAVING")?;
        }
        let projected = output.schema();
        for order in &block.order_by {
            self.alias_expression(&order.expr, &projected, &input, &block.subqueries)?;
        }
        for expression in block
            .group_by
            .iter()
            .chain(block.grouping_sets.iter().flatten())
        {
            let expression =
                crate::semantics::grouping_sets::resolve_grouping_expression_reference(
                    self.routines,
                    expression,
                    &block.projections,
                    &source,
                    outer,
                    &self.parameters.values(),
                )?;
            let mut value = self.expression(expression, &input, &block.subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
        }
        for expression in &block.distinct_on {
            self.alias_expression(expression, &projected, &input, &block.subqueries)?;
        }
        self.slice(
            block.limit.as_ref(),
            block.offset.as_ref(),
            &input,
            &block.subqueries,
        )?;
        self.query_windows(block, &input)?;
        crate::semantics::grouping_sets::validate_grouped_expressions(
            self.routines,
            block,
            &source,
            outer,
            &self.parameters.values(),
        )?;
        if !preserve_unknown {
            self.resolve_targets(&mut output)?;
        }
        Ok(output)
    }

    fn query_windows(&mut self, block: &QueryBlockPlan, input: &RowSchema) -> Result<(), SQLError> {
        let mut order_types = Vec::with_capacity(block.windows.len());
        for (slot, window) in block.windows.iter().enumerate() {
            let inherited = window
                .inherited
                .map(|parent| {
                    order_types
                        .get(parent)
                        .filter(|_| parent < slot)
                        .ok_or_else(|| {
                            SQLError::Internal(
                                "window inheritance is outside its query block".into(),
                            )
                        })
                })
                .transpose()?;
            let order =
                self.window_definition(&window.spec, input, &block.subqueries, inherited)?;
            order_types.push(order);
        }
        let mut result = Ok(());
        for expression in block
            .projections
            .iter()
            .map(|projection| &projection.expr)
            .chain(block.order_by.iter().map(|order| &order.expr))
        {
            expression.visit(&mut |node| {
                if result.is_ok() {
                    if let ScalarExpr::WindowCall { spec, .. } = node {
                        if spec.definition.is_none() {
                            result = self.window_specification(spec, input, &block.subqueries);
                        } else if spec
                            .definition
                            .is_some_and(|slot| slot >= block.windows.len())
                        {
                            result = Err(SQLError::Internal(
                                "window call is outside its query block".into(),
                            ));
                        }
                    }
                }
            });
        }
        result
    }

    pub(super) fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        source: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<QueryOutput, SQLError> {
        let labels = projection_columns(projections);
        let mut output = QueryOutput {
            columns: Vec::new(),
            types: Vec::new(),
            open: false,
        };
        for (projection, label) in projections.iter().zip(labels) {
            let expansion = if matches!(projection.expr, ScalarExpr::QualifiedStar(_)) {
                input
            } else {
                source
            };
            if let Some(columns) = projection_star_columns(&projection.expr, expansion)? {
                output.open |= match &projection.expr {
                    ScalarExpr::QualifiedStar(qualifier) => {
                        expansion.columns_are_open(Some(qualifier))
                    }
                    _ => expansion.columns_are_open(None),
                };
                let fields = super::super::record_fields::star_fields(&projection.expr, expansion)
                    .unwrap_or_default();
                for ((column, ty), fields) in columns.into_iter().zip(fields) {
                    output.columns.push(column);
                    let mut value = ExpressionType::resolved(ty);
                    value.record_fields = fields;
                    output.types.push(value);
                }
            } else {
                output.columns.push(label);
                output
                    .types
                    .push(self.expression(&projection.expr, input, subqueries)?);
            }
        }
        Ok(output)
    }

    pub(super) fn resolve_targets(&mut self, output: &mut QueryOutput) -> Result<(), SQLError> {
        for ty in &mut output.types {
            self.parameters.coerce_unknown(ty, &ColumnType::Text)?;
        }
        Ok(())
    }

    pub(super) fn values(
        &mut self,
        rows: &[Vec<ScalarExpr>],
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<QueryOutput, SQLError> {
        let width = rows.first().map_or(0, Vec::len);
        let mut columns = vec![Vec::new(); width];
        let empty = RowSchema::default();
        let input = outer.unwrap_or(&empty);
        for row in rows {
            if row.len() != width {
                return Err(error(
                    "42601",
                    "VALUES lists must all be the same length".into(),
                ));
            }
            for (column, expression) in columns.iter_mut().zip(row) {
                column.push(self.expression(expression, input, subqueries)?);
            }
        }
        let types = columns
            .iter_mut()
            .map(|column| {
                let ty = self.common(crate::type_resolution::CommonTypeContext::Values, column)?;
                let mut value = ExpressionType::resolved(ty);
                value.record_fields = super::super::record_fields::common_fields(
                    column.iter().map(|item| item.record_fields.as_ref()),
                );
                Ok::<ExpressionType, SQLError>(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueryOutput {
            open: false,
            columns: (1..=width)
                .map(|position| format!("column{position}"))
                .collect(),
            types,
        })
    }

    fn alias_expression(
        &mut self,
        expression: &ScalarExpr,
        primary: &RowSchema,
        fallback: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut value = if matches!(expression, ScalarExpr::Column(_) | ScalarExpr::Position(_)) {
            let input = overlay_outer_schema(primary, Some(fallback));
            self.expression(expression, &input, subqueries)?
        } else {
            self.expression(expression, fallback, subqueries)?
        };
        self.parameters
            .coerce_unknown(&mut value, &ColumnType::Text)?;
        Ok(())
    }

    fn slice(
        &mut self,
        limit: Option<&ScalarExpr>,
        offset: Option<&ScalarExpr>,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        for (expression, context) in offset
            .map(|value| (value, "OFFSET"))
            .into_iter()
            .chain(limit.map(|value| (value, "LIMIT")))
        {
            let mut value = self.expression(expression, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::BigInteger)?;
            let ty = value.ty.as_ref().expect("coerced slice expression");
            if !crate::assignment_type_compatible(ty, &ColumnType::BigInteger) {
                return Err(error(
                    "42804",
                    format!(
                        "argument of {context} must be type bigint, not type {}",
                        ty.regtype_name()
                    ),
                ));
            }
            if analysis::references_local_column(expression, input) {
                return Err(error(
                    "42P10",
                    format!("argument of {context} must not contain variables"),
                ));
            }
        }
        Ok(())
    }
}
