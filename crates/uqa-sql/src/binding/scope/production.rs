//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical scope composition carries the invoking allowance through every newly constructed namespace.

use super::{source_namespace, RowSchema, SQLError, SourceScope};
use crate::{
    ast::JoinKind,
    plan::{CommandPlan, SourcePlan},
};
use uqa_core::memory::ProductionControl;

fn empty(control: &ProductionControl<'_>) -> Result<RowSchema, SQLError> {
    RowSchema::empty_with_control(control)
}

pub fn overlay_outer_schema_with_control(
    current: &RowSchema,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    match outer {
        Some(outer) => RowSchema::with_outer_schema_with_control(current, outer, control),
        None => Ok(current.clone()),
    }
}

impl SourceScope<'_> {
    pub fn input_with_control(
        self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        let Some(preceding) = self.preceding else {
            return Ok(self.outer.cloned());
        };
        let active = self.inherited_lateral
            || matches!(
                source,
                SourcePlan::Function { .. }
                    | SourcePlan::FunctionGroup { .. }
                    | SourcePlan::Values { lateral: true, .. }
                    | SourcePlan::Subquery { lateral: true, .. }
            );
        Ok(Some(if active {
            overlay_outer_schema_with_control(preceding, self.outer, control)?
        } else {
            let outer;
            let input = if let Some(input) = self.outer {
                input
            } else {
                outer = empty(control)?;
                &outer
            };
            RowSchema::with_unavailable_names_with_control(input, preceding, true, control)?
        }))
    }
    pub fn right_namespace_with_control(
        self,
        source: &SourcePlan,
        left: &RowSchema,
        kind: JoinKind,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        control.check()?;
        let left = if matches!(kind, JoinKind::Right | JoinKind::Full) {
            RowSchema::without_lateral_access_with_control(left, source_namespace(source), control)?
        } else {
            left.clone()
        };
        match self.preceding {
            Some(preceding) => RowSchema::with_peer_scope_with_control(preceding, &left, control),
            None => Ok(left),
        }
    }
    pub fn condition_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        match self.preceding {
            None => Ok(self.outer.cloned()),
            Some(preceding) => {
                let outer;
                let input = if let Some(input) = self.outer {
                    input
                } else {
                    outer = empty(control)?;
                    &outer
                };
                Ok(Some(RowSchema::with_unavailable_names_with_control(
                    input, preceding, false, control,
                )?))
            }
        }
    }
    pub fn describe_output_with_control(
        source: &SourcePlan,
        schema: RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        control.check()?;
        match source {
            SourcePlan::Join { alias: Some(_), .. } => {
                RowSchema::with_join_diagnostic_namespace_with_control(
                    &schema,
                    source_namespace(source),
                    control,
                )
            }
            SourcePlan::Join { alias: None, .. } | SourcePlan::RetainedRows(_) => Ok(schema),
            _ => RowSchema::with_source_diagnostic_namespace_with_control(
                &schema,
                source_namespace(source),
                control,
            ),
        }
    }
    pub fn command_target_with_control(
        command: &CommandPlan,
        target: &RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<Option<RowSchema>, SQLError> {
        control.check()?;
        if !matches!(command, CommandPlan::Update(_) | CommandPlan::Delete(_)) {
            return Ok(None);
        }
        let namespace = command
            .target_qualifier()
            .expect("mutation target qualifier");
        let target = RowSchema::with_relation_qualifier_with_control(target, namespace, control)?;
        let target =
            RowSchema::with_source_diagnostic_namespace_with_control(&target, namespace, control)?;
        Ok(Some(RowSchema::without_target_lateral_access_with_control(
            &target, namespace, control,
        )?))
    }
}
