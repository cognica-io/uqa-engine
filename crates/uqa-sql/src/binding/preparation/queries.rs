//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query analysis follows `PostgreSQL`'s source, target, clause, and coercion order.

use super::super::{analysis, overlay_outer_schema, projection_star_columns};
use super::{
    error, ColumnType, ExpressionType, Preparation, QueryOutput, QueryPlan, RowSchema, SQLError,
    ScalarExpr,
};
use crate::plan::{ProjectionPlan, QueryBlockPlan, RelationalPlan};
use crate::semantics::sets::placement::SetPlacement;

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
        self.in_query_level(|analysis| analysis.query_level_output(plan, outer, preserve_unknown))
    }

    fn query_level_output(
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
            let output = result?;
            self.parameters.collations.validate_query(plan)?;
            for value in &output.types {
                value.derivation.validate()?;
            }
            Ok(output)
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
                let mut left_output = self.query_output(left, outer, true)?;
                let mut right_output = self.query_output(right, outer, true)?;
                left_output.retain_set_operand_names(left);
                right_output.retain_set_operand_names(right);
                let output = self.set_output(left_output, right_output, *kind, *all)?;
                self.set_ordering(order_by, &output, subqueries)?;
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
        let allow_indeterminate = matches!((kind, all), (crate::ast::SetOpKind::Union, true));
        for (left, right) in left.types.iter_mut().zip(right.types) {
            let mut values = [
                std::mem::replace(left, ExpressionType::resolved(None)),
                right,
            ];
            let ty = self.common(
                crate::type_resolution::CommonTypeContext::set_operation(kind),
                &mut values,
            )?;
            if let Some(target) = &ty {
                for value in &mut values {
                    value.derivation = value.derivation.coerce(value.ty.as_ref(), target);
                }
            }
            let mut result = ExpressionType::combined(ty, &values);
            let oid = result.derivation.common_oid(allow_indeterminate)?;
            if !allow_indeterminate {
                if let Some(ty) = &result.ty {
                    crate::type_resolution::ComparisonSupport::Equality
                        .require_in_catalog(ty, self.routines)?;
                }
            }
            // An enclosing set operation receives an output column, not an explicit leaf expression.
            result.derivation = crate::catalog::collations::derivation::Derivation::implicit(oid);
            result.record_fields = super::super::record_fields::common_fields(
                values.iter().map(|value| value.record_fields.as_ref()),
            );
            result
                .record_descriptor
                .clone_from(&values[0].record_descriptor);
            *left = result;
        }
        left.key_origins.clear();
        left.set_sources.append(&mut right.set_sources);
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
        self.block_with_source(block, &source, outer, preserve_unknown)
    }

    pub(super) fn block_with_source(
        &mut self,
        block: &QueryBlockPlan,
        source: &RowSchema,
        outer: Option<&RowSchema>,
        preserve_unknown: bool,
    ) -> Result<QueryOutput, SQLError> {
        self.in_query_level(|analysis| {
            analysis.block_level_output(block, source, outer, preserve_unknown)
        })
    }

    fn block_level_output(
        &mut self,
        block: &QueryBlockPlan,
        source: &RowSchema,
        outer: Option<&RowSchema>,
        preserve_unknown: bool,
    ) -> Result<QueryOutput, SQLError> {
        let input = overlay_outer_schema(source, outer);
        let input = self.scope.with_stored_outer_internal_aliases(&input);
        let mut output = self.projections(&block.projections, source, &input, &block.subqueries)?;
        if let Some(predicate) = &block.r#where {
            self.in_placement(SetPlacement::Where, |analysis| {
                analysis.require_boolean(predicate, &input, &block.subqueries, "WHERE")
            })?;
        }
        if let Some(having) = &block.having {
            self.in_placement(SetPlacement::Having, |analysis| {
                analysis.require_boolean(having, &input, &block.subqueries, "HAVING")
            })?;
        }
        let projected = output.schema();
        for order in &block.order_by {
            let value = self.alias_expression(
                &order.expr,
                &output,
                &projected,
                &input,
                &block.subqueries,
                "ORDER BY",
            )?;
            if let Some(ty) = &value.ty {
                crate::type_resolution::ComparisonSupport::Ordering
                    .require_in_catalog(ty, self.routines)?;
            }
            self.query_key(&order.expr, value.derivation, block, &input)?;
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
                    source,
                    outer,
                    &self.parameters.values(),
                )?;
            let mut value = self.expression(expression, &input, &block.subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
            if let Some(ty) = &value.ty {
                crate::type_resolution::ComparisonSupport::Equality
                    .require_in_catalog(ty, self.routines)?;
            }
            self.query_key(expression, value.derivation, block, &input)?;
        }
        for expression in &block.distinct_on {
            let value = self.alias_expression(
                expression,
                &output,
                &projected,
                &input,
                &block.subqueries,
                "DISTINCT ON",
            )?;
            if let Some(ty) = &value.ty {
                crate::type_resolution::ComparisonSupport::Equality
                    .require_in_catalog(ty, self.routines)?;
            }
            self.query_key(expression, value.derivation, block, &input)?;
        }
        if block.distinct && block.distinct_on.is_empty() {
            for (position, value) in output.types.iter().enumerate() {
                if let Some(ty) = &value.ty {
                    crate::type_resolution::ComparisonSupport::Equality
                        .require_in_catalog(ty, self.routines)?;
                }
                if let Some(origin) = output.key_origin(position) {
                    self.parameters.collations.require_key_at(origin);
                }
            }
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
            source,
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

    pub(super) fn projections<'a>(
        &mut self,
        projections: impl IntoIterator<Item = &'a ProjectionPlan>,
        source: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<QueryOutput, SQLError> {
        let mut output = QueryOutput {
            columns: Vec::new(),
            types: Vec::new(),
            open: false,
            key_origins: Vec::new(),
            set_sources: Vec::new(),
        };
        for projection in projections {
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
                let fields =
                    super::super::record_fields::star_metadata(&projection.expr, expansion)
                        .unwrap_or_default();
                for ((column, ty, collation), fields) in columns.into_iter().zip(fields) {
                    output.columns.push(column);
                    let mut value = ExpressionType::resolved(ty);
                    if let Some(oid) = collation {
                        value.derivation =
                            crate::catalog::collations::derivation::Derivation::implicit(
                                i64::from(oid),
                            );
                    }
                    if let Some(fields) = fields {
                        value.record_fields = fields.types;
                        value.record_descriptor = fields.descriptor;
                    }
                    output.types.push(value);
                }
            } else {
                let mut value = self.expression(&projection.expr, input, subqueries)?;
                if matches!(value.ty, Some(ColumnType::Composite(_))) {
                    value.record_fields = self.scope.bind_record_fields(
                        self.routines,
                        &projection.expr,
                        input,
                        subqueries,
                        &self.parameters.values(),
                    )?;
                }
                if matches!(
                    value.derivation,
                    crate::catalog::collations::derivation::Derivation::Conflict(..)
                ) {
                    output.key_origins.push((
                        output.types.len(),
                        std::ptr::NonNull::from(&projection.expr),
                    ));
                    self.parameters
                        .collations
                        .record_target(&projection.expr, value.derivation);
                }
                output
                    .columns
                    .push(crate::semantics::projection_label_at(projection));
                output.types.push(value);
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
            for (position, expression) in row.iter().enumerate() {
                let value = self.in_placement(SetPlacement::Values, |analysis| {
                    analysis.expression(expression, input, subqueries)
                })?;
                if let Some(column) = columns.get_mut(position) {
                    column.push(value);
                }
            }
            if row.len() != width {
                return Err(error(
                    "42601",
                    "VALUES lists must all be the same length".into(),
                ));
            }
        }
        let types = columns
            .iter_mut()
            .map(|column| {
                let ty = self.common(crate::type_resolution::CommonTypeContext::Values, column)?;
                let mut value = ExpressionType::combined(ty, column);
                // VALUES resolves each column after all row expressions have been analyzed, before proceeding to the next column.
                let oid = value.derivation.common_oid(true)?;
                // The VALUES relation exports a column selection, not its input expression's explicit strength.
                value.derivation =
                    crate::catalog::collations::derivation::Derivation::implicit(oid);
                value.record_fields = super::super::record_fields::common_fields(
                    column.iter().map(|item| item.record_fields.as_ref()),
                );
                Ok::<ExpressionType, SQLError>(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueryOutput {
            open: false,
            key_origins: Vec::new(),
            set_sources: Vec::new(),
            columns: (1..=width)
                .map(|position| format!("column{position}"))
                .collect(),
            types,
        })
    }

    pub(super) fn alias_expression(
        &mut self,
        expression: &ScalarExpr,
        output: &QueryOutput,
        primary: &RowSchema,
        fallback: &RowSchema,
        subqueries: &[QueryPlan],
        clause: &str,
    ) -> Result<ExpressionType, SQLError> {
        let index = match expression {
            ScalarExpr::Literal(uqa_core::Value::Int(position)) => Some(
                crate::semantics::query_output_position(*position, primary.len(), clause)?,
            ),
            ScalarExpr::Column(name) if !primary.column_is_ambiguous(name) => {
                primary.column_slot(name)
            }
            ScalarExpr::Position(position) if *position < output.types.len() => Some(*position),
            _ => None,
        };
        if let Some(index) = index.filter(|index| *index < output.types.len()) {
            if let Some(origin) = output.key_origin(index) {
                self.parameters.collations.require_key_at(origin);
            }
            let mut value = ExpressionType::resolved(output.types[index].ty.clone());
            value.derivation = output.types[index].derivation;
            return Ok(value);
        }
        let mut value = if matches!(expression, ScalarExpr::Column(_) | ScalarExpr::Position(_)) {
            let input = overlay_outer_schema(primary, Some(fallback));
            self.expression(expression, &input, subqueries)?
        } else {
            self.expression(expression, fallback, subqueries)?
        };
        self.parameters
            .coerce_unknown(&mut value, &ColumnType::Text)?;
        Ok(value)
    }

    fn query_key(
        &mut self,
        expression: &ScalarExpr,
        selected: crate::catalog::collations::derivation::Derivation,
        block: &QueryBlockPlan,
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        use crate::catalog::collations::derivation::Derivation;
        if !matches!(selected, Derivation::Conflict(..)) {
            return Ok(());
        }
        self.parameters.collations.require_key(expression, selected);
        let mut identity = None;
        for projection in &block.projections {
            if !self
                .parameters
                .collations
                .indeterminate_target(&projection.expr)
            {
                continue;
            }
            let same = if projection.expr == *expression {
                true
            } else {
                let parameters = self.parameters.values();
                let key = match &identity {
                    Some(key) => key,
                    None => identity.insert(crate::semantics::grouping_sets::expression_identity(
                        self.routines,
                        expression,
                        input,
                        &parameters,
                    )?),
                };
                *key == crate::semantics::grouping_sets::expression_identity(
                    self.routines,
                    &projection.expr,
                    input,
                    &parameters,
                )?
            };
            if same {
                self.parameters
                    .collations
                    .require_key(&projection.expr, selected);
                break;
            }
        }
        Ok(())
    }

    fn slice(
        &mut self,
        limit: Option<&ScalarExpr>,
        offset: Option<&ScalarExpr>,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        for (expression, context, placement) in offset
            .map(|value| (value, "OFFSET", SetPlacement::Offset))
            .into_iter()
            .chain(limit.map(|value| (value, "LIMIT", SetPlacement::Limit)))
        {
            let mut value = self.in_placement(placement, |analysis| {
                analysis.expression(expression, input, subqueries)
            })?;
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
