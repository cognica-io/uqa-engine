//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! VALUES action inputs preserve per-row DEFAULT flags and ordered default-query namespaces.

use super::{
    default_expression, input_columns, insert, overrides_identity, Buffer, InputColumns,
    ValuesInput,
};
use crate::{
    ast::{Expr, InternalRelationId},
    catalog::roles::RoleReference,
    plan::{
        copy::copy_syntax_with_control, ExpressionPlan, MutableCommandPlan, MutableQueryPlan,
        MutableSourcePlan, OwnedProjection, OwnedScalarExpr, ScalarQueryArena,
    },
    semantics::rules::analysis::RuleAnalysisContext,
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

impl ValuesInput {
    /// Convert a VALUES action into its query input before attaching the event relation. Existing INSERT SELECT sources already own their query and remain unchanged.
    pub fn prepare_action_with_control(
        context: RuleAnalysisContext<'_>,
        command: MutableCommandPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableCommandPlan, SQLError> {
        control.check()?;
        if insert(&command)?.source.is_some() {
            return Ok(command);
        }
        let values =
            Self::describe_with_control(insert(&command)?, control)?.expect("VALUES action");
        let (command, columns) = input_columns(context, command, values.width, control)?;
        let (command, source) = command.take_insert_rows_query(control)?;
        let source = values.source_with_control(source, &columns, control)?;
        drop(columns);
        let command = command.map_insert_source(control, |_, previous| {
            debug_assert!(previous.is_none());
            Ok(source)
        })?;
        values.project_layer_with_control(context, command, control)
    }

    fn source_with_control(
        &self,
        source: MutableQueryPlan,
        columns: &InputColumns,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        if self.inline {
            return source.project_values(control);
        }
        source.map_value_rows(control, |row| {
            let mut flags = Buffer::new(control);
            let mut output = Buffer::new(control);
            for (position, value) in row.enumerate() {
                let is_default = matches!(&*value, ScalarExpr::Default);
                if self.defaults.contains(&position) {
                    flags.push(is_default, control)?;
                }
                let ty = &columns.selected[position].1;
                let value = if is_default {
                    drop(value);
                    OwnedScalarExpr::typed_literal(
                        control.copy_value(&Value::Null)?,
                        ty.catalog_name_with_control(control)?,
                        Some(ty.clone_with_control(control)?),
                        None,
                        None,
                        control,
                    )?
                } else {
                    cast(value, ty, control)?
                };
                output.push(value, control)?;
            }
            for flag in flags {
                output.push(
                    ScalarExpr::Literal(Value::Bool(flag)).copy_for_rewrite(control)?,
                    control,
                )?;
            }
            Ok(output.into_iter())
        })
    }

    fn project_layer_with_control(
        &self,
        context: RuleAnalysisContext<'_>,
        command: MutableCommandPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableCommandPlan, SQLError> {
        let (command, columns) = input_columns(context, command, self.width, control)?;
        let overriding = insert(&command)?.overriding;
        let mut defaults = Buffer::new(control);
        let mut resolved = Buffer::new(control);
        for (index, _) in &*columns.selected {
            let column = &columns.definitions[*index];
            let default = if overrides_identity(column, overriding) {
                Some(copy_syntax_with_control(
                    &Expr::Literal(Value::Null),
                    control,
                )?)
            } else {
                default_expression(context, column, control)?
            };
            resolved.push(self.inline && default.is_some(), control)?;
            defaults.push(default, control)?;
        }
        let metadata = self.copy_resolved(&resolved, control)?;
        let command = command.map_insert_source(control, |_, source| {
            let source = source.expect("VALUES input query");
            if self.inline {
                self.project_inline(source, &columns, &defaults, control)
            } else {
                self.project_default_rows(source, &columns, &defaults, control)
            }
        })?;
        command.set_rule_values_input(metadata, control)
    }
    fn project_inline(
        &self,
        source: MutableQueryPlan,
        columns: &InputColumns,
        defaults: &[Option<Produced<Expr>>],
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        let subject = source
            .relation_privilege_subject
            .as_ref()
            .map(|subject| subject.clone_with_control(control))
            .transpose()?;
        let source = source.map_projection_inputs(control, |projections, arena| {
            let mut output = Buffer::new(control);
            for (position, mut projection) in projections.enumerate() {
                if self.unresolved.contains(&position) {
                    drop(projection.expression);
                    projection.expression =
                        default_value(defaults[position].as_ref(), arena, control)?;
                }
                projection.expression = cast(
                    projection.expression,
                    &columns.selected[position].1,
                    control,
                )?;
                output.push(projection, control)?;
            }
            retain_authority(arena, subject.as_deref(), control)?;
            Ok(output.into_iter())
        })?;
        source.clear_row_analysis(control)
    }

    fn project_default_rows(
        &self,
        source: MutableQueryPlan,
        columns: &InputColumns,
        defaults: &[Option<Produced<Expr>>],
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        if self.defaults.is_empty() {
            return Ok(source);
        }
        let subject = source
            .relation_privilege_subject
            .as_ref()
            .map(|subject| subject.clone_with_control(control))
            .transpose()?;
        let bound = source.relations_bound;
        let relation = InternalRelationId::allocate();
        let result = MutableQueryPlan::projected(std::iter::empty(), control)?;
        let result = result.map_projection_inputs(control, |_, arena| {
            let mut output = Buffer::new(control);
            for (position, (_, ty)) in columns.selected.iter().enumerate() {
                let value = ScalarExpr::InternalColumn(relation.column(position))
                    .copy_for_rewrite(control)?;
                let expression = if let Some(flag) =
                    self.defaults.iter().position(|value| *value == position)
                {
                    let condition = ScalarExpr::InternalColumn(relation.column(self.width + flag))
                        .copy_for_rewrite(control)?;
                    let default = cast(
                        default_value(defaults[position].as_ref(), arena, control)?,
                        ty,
                        control,
                    )?;
                    OwnedScalarExpr::case(
                        None,
                        std::iter::once((condition, default)),
                        Some(value),
                        control,
                    )?
                } else {
                    value
                };
                output.push(
                    OwnedProjection {
                        expression,
                        alias: None,
                    },
                    control,
                )?;
            }
            for (flag, position) in self.defaults.iter().enumerate() {
                let expression = if defaults[*position].is_some() {
                    ScalarExpr::Literal(Value::Bool(false))
                } else {
                    ScalarExpr::InternalColumn(relation.column(self.width + flag))
                }
                .copy_for_rewrite(control)?;
                output.push(
                    OwnedProjection {
                        expression,
                        alias: None,
                    },
                    control,
                )?;
            }
            retain_authority(arena, subject.as_deref(), control)?;
            Ok(output.into_iter())
        })?;
        let source = MutableSourcePlan::subquery(
            source,
            None,
            control.finish(Vec::new(), control.empty_reservation())?,
            Some(relation),
            false,
            control,
        )?;
        result
            .set_privilege_subject(subject, control)?
            .set_relations_bound(bound, control)?
            .replace_source(Some(source), control)
    }
}

fn default_value(
    default: Option<&Produced<Expr>>,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let expression = default.map_or(&Expr::Literal(Value::Null), |default| &**default);
    arena.append_expression(
        ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?,
        control,
    )
}

fn cast(
    value: OwnedScalarExpr,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::cast(
        value,
        ty.catalog_name_with_control(control)?,
        true,
        control,
    )?)
}

pub(in crate::semantics::rules::binding::actions::query) fn retain_authority(
    arena: &mut ScalarQueryArena,
    subject: Option<&RoleReference>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if let Some(subject) = subject {
        for index in 0..arena.queries().len() {
            if arena.queries()[index].relation_privilege_subject.is_none() {
                arena.map_query(index, control, |query| {
                    query.set_privilege_subject(Some(subject.clone_with_control(control)?), control)
                })?;
            }
        }
    }
    Ok(())
}
