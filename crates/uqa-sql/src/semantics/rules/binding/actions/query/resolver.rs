//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native event templates own their syntax and deferred diagnostics independently of executable query inputs.

use crate::{
    ast::{DeferredSQLError, Expr, InternalColumnRef, InternalRelationId},
    plan::{
        copy::SyntaxProduction, input_projection::OwnedInputProjections, ExpressionPlan,
        InsertPlan, MutableExpressionPlan, RelationalPlan,
    },
    plpgsql::{ResolvedVariable, RewriteReference, VariableResolver},
    semantics::rules::analysis::{rule_returning_columns_with_control, RuleAnalysisContext},
    SQLError,
};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

mod conditions;
mod mutation;

struct RuleColumnTemplate {
    name: String,
    old: Expr,
    new: Result<Expr, DeferredSQLError>,
}

pub struct RuleInputColumns {
    pub positions: Produced<Vec<usize>>,
    pub width: usize,
}

pub struct OwnedRuleQueryResolver {
    table: Produced<String>,
    columns: Produced<Vec<RuleColumnTemplate>>,
    pub expressions: OwnedInputProjections,
}

impl OwnedRuleQueryResolver {
    pub(in crate::semantics::rules::binding::actions::query) fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn reserved_bytes(&self) -> usize {
        self.table.reserved_bytes()
            + self.columns.reserved_bytes()
            + self.expressions.allocated_bytes()
            + self
                .expressions
                .values()
                .map(Produced::reserved_bytes)
                .sum::<usize>()
    }

    pub fn input_columns_with_control(
        &self,
        context: RuleAnalysisContext<'_>,
        insert: &InsertPlan,
        action: &crate::ast::Statement,
        condition: Option<&Expr>,
        target_columns: &dyn crate::semantics::rules::action_binding::ActionColumns,
        control: &ProductionControl<'_>,
    ) -> Result<Option<RuleInputColumns>, SQLError> {
        let Some(required) =
            crate::semantics::rules::action_binding::rule_action_input_columns_with_control(
                context.sources,
                action,
                condition,
                target_columns,
                control,
            )?
        else {
            return Ok(None);
        };
        let width = if insert.columns.is_empty() {
            insert
                .source
                .as_deref()
                .and_then(|query| query.analyzed_output.as_ref())
                .map_or_else(
                    || super::inline_width(insert).unwrap_or(self.columns.len()),
                    crate::RowSchema::len,
                )
                .min(self.columns.len())
        } else {
            insert.columns.len()
        };
        let mut positions = uqa_core::memory::ProductionVec::new(*control);
        for position in 0..width {
            control.check()?;
            let name = if insert.columns.is_empty() {
                &self.columns[position].name
            } else {
                &insert.columns[position].column
            };
            if required.binary_search(name).is_ok() {
                positions.push_copy(position)?;
            }
        }
        Ok(Some(RuleInputColumns {
            positions: positions.finish()?,
            width,
        }))
    }

    pub fn from_insert_with_control(
        context: RuleAnalysisContext<'_>,
        insert: &InsertPlan,
        relation: InternalRelationId,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let definitions = rule_returning_columns_with_control(context, &insert.table, control)?;
        let width = insert
            .source
            .as_deref()
            .and_then(|source| source.analyzed_output.as_ref())
            .map_or_else(
                || super::inline_width(insert).unwrap_or(definitions.len()),
                crate::RowSchema::len,
            );
        let output = SyntaxProduction::new(*control);
        let mut columns = output.vector(definitions.len())?;
        let mut pending = crate::production::Buffer::new(control);
        for column in &*definitions {
            output.check()?;
            let ty = column.ty.catalog_name_with_control(control)?;
            let old = Expr::Cast {
                implicit: true,
                expr: output.boxed(Expr::Literal(uqa_core::Value::Null))?,
                ty: output.take(ty),
            };
            let expression = insert_column(
                context,
                insert,
                &definitions,
                column,
                relation,
                width,
                control,
            )?;
            let expression = bind_column_collation(expression, column, control)?;
            let reference = InternalRelationId::allocate().column(0);
            pending.push((reference, expression), control)?;
            columns.push(RuleColumnTemplate {
                name: output.text(&column.name)?,
                old,
                new: Ok(Expr::InternalColumn(reference)),
            })?;
        }
        let mut inputs = inline_inputs(insert, relation, control)?;
        let subject = insert
            .source
            .as_ref()
            .and_then(|source| source.relation_privilege_subject.as_ref());
        let mut expressions = OwnedMap::new();
        for (reference, expression) in pending {
            let expression = crate::plan::input_projection::substitute_expression_inputs_owned(
                expression, &inputs, control,
            )?;
            let expression = retain_authority(expression, subject, control)?;
            insert_expression(
                &mut expressions,
                reference,
                expression.finish(control)?,
                control,
            )?;
        }
        for (reference, expression) in std::mem::take(&mut inputs) {
            insert_expression(&mut expressions, reference, expression, control)?;
        }
        let columns = columns.finish()?;
        let columns = output.finish(columns)?;
        Ok(Self {
            table: control.copy_text(&insert.table)?,
            columns,
            expressions,
        })
    }
}

fn insert_column(
    context: RuleAnalysisContext<'_>,
    insert: &InsertPlan,
    definitions: &[crate::ast::ColumnDef],
    column: &crate::ast::ColumnDef,
    relation: InternalRelationId,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    if insert
        .columns
        .iter()
        .any(|target| target.column == column.name && !target.is_whole_column())
    {
        return super::assignments::column_expression_with_control(
            context, insert, column, relation, control,
        );
    }
    let position = if insert.columns.is_empty() {
        definitions
            .iter()
            .take(width)
            .position(|candidate| candidate.name == column.name)
    } else {
        insert
            .columns
            .iter()
            .position(|target| target.column == column.name)
    }
    .filter(|_| !super::values::overrides_identity(column, insert.overriding));
    let value = if let Some(position) = position {
        ExpressionPlan::lower_borrowed_for_rewrite(
            &Expr::InternalColumn(relation.column(position)),
            control,
        )?
    } else {
        let default = super::values::default_expression_with_control(context, column, control)?;
        ExpressionPlan::lower_borrowed_for_rewrite(
            default
                .as_deref()
                .unwrap_or(&Expr::Literal(uqa_core::Value::Null)),
            control,
        )?
    };
    value.map_scalar(control, |scalar, _| {
        Ok(crate::plan::OwnedScalarExpr::cast(
            scalar,
            column.ty.catalog_name_with_control(control)?,
            true,
            control,
        )?)
    })
}

pub(super) fn insert_expression(
    output: &mut OwnedInputProjections,
    reference: InternalColumnRef,
    expression: Produced<ExpressionPlan>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        output.insert_budgeted(reference, expression, budget)?;
    } else {
        output.insert(reference, expression);
    }
    Ok(())
}

fn inline_inputs(
    insert: &InsertPlan,
    relation: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<OwnedInputProjections, SQLError> {
    let mut inputs = OwnedMap::new();
    if !insert
        .rule_values_input
        .as_ref()
        .is_some_and(crate::plan::RuleValuesInput::is_inline)
    {
        return Ok(inputs);
    }
    let source = insert.source.as_ref().expect("inline input query");
    let RelationalPlan::QueryBlock(block) = &source.root else {
        return Err(SQLError::Internal(
            "rule target list lost its query block".into(),
        ));
    };
    for (position, projection) in block.projections.iter().enumerate() {
        let expression =
            MutableExpressionPlan::copy_from(&projection.expr, &block.subqueries, control)?
                .prune_subqueries(control)?;
        let expression = retain_authority(
            expression,
            source.relation_privilege_subject.as_ref(),
            control,
        )?;
        insert_expression(
            &mut inputs,
            relation.column(position),
            expression.finish(control)?,
            control,
        )?;
    }
    Ok(inputs)
}

pub(super) fn retain_authority(
    mut expression: MutableExpressionPlan,
    subject: Option<&crate::catalog::roles::RoleReference>,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    if let Some(subject) = subject {
        for index in 0..expression.queries().len() {
            if expression.queries()[index]
                .relation_privilege_subject
                .is_none()
            {
                expression = expression.map_query(index, control, |query| {
                    query.set_privilege_subject(Some(subject.clone_with_control(control)?), control)
                })?;
            }
        }
    }
    Ok(expression)
}

pub(super) fn bind_column_collation(
    expression: MutableExpressionPlan,
    column: &crate::ast::ColumnDef,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    if !crate::schema::composites::type_is_collatable(&column.ty) {
        return Ok(expression);
    }
    let oid = column.collation_oid.unwrap_or_else(|| {
        u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
            &column.ty,
        ))
        .expect("type collation identity")
    });
    let collation = crate::ir::ResultCollation::new(
        crate::catalog::collations::derivation::Derivation::implicit(i64::from(oid)),
        oid,
    );
    expression.map_scalar(control, |scalar, _| {
        Ok(crate::plan::OwnedScalarExpr::collated(
            scalar,
            column.ty.clone_with_control(control)?,
            collation,
            control,
        )?)
    })
}

impl VariableResolver for OwnedRuleQueryResolver {
    fn rewrite_with_control(
        &mut self,
        reference: RewriteReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Expr>>, SQLError> {
        control.check()?;
        let output = SyntaxProduction::new(*control);
        let selected = |column: &RuleColumnTemplate, new: bool| -> Result<Expr, SQLError> {
            if new {
                output.copy(
                    column
                        .new
                        .as_ref()
                        .map_err(|error| SQLError::from(error.clone()))?,
                )
            } else {
                output.copy(&column.old)
            }
        };
        let expression = match reference {
            RewriteReference::Name(name) | RewriteReference::WholeRow(name) => {
                let new = name.eq_ignore_ascii_case("new");
                if !new && !name.eq_ignore_ascii_case("old") {
                    return Ok(None);
                }
                let mut items = output.vector(self.columns.len())?;
                for column in &*self.columns {
                    items.push(selected(column, new)?)?;
                }
                Expr::Cast {
                    implicit: true,
                    expr: output.boxed(Expr::Row(items.finish()?))?,
                    ty: output.text(&self.table)?,
                }
            }
            RewriteReference::Qualified { qualifier, column } => {
                let new = qualifier.eq_ignore_ascii_case("new");
                if !new && !qualifier.eq_ignore_ascii_case("old") {
                    return Ok(None);
                }
                let selected_column = self
                    .columns
                    .iter()
                    .find(|value| value.name == column)
                    .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))?;
                selected(selected_column, new)?
            }
            RewriteReference::Parameter(_) | RewriteReference::Internal(_) => return Ok(None),
        };
        Ok(Some(output.finish(expression)?))
    }
    fn resolve_name(&mut self, _: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_qualified(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_param(&mut self, _: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .rewrite_with_control(
                RewriteReference::Name(name),
                &ProductionControl::uncontrolled(),
            )?
            .map(|value| value.into_uncontrolled().expect("ordinary reference")))
    }
    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .rewrite_with_control(
                RewriteReference::Qualified { qualifier, column },
                &ProductionControl::uncontrolled(),
            )?
            .map(|value| value.into_uncontrolled().expect("ordinary reference")))
    }
    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.rewrite_name(qualifier)
    }
}
