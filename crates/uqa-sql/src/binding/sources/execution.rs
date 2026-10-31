//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution source binding consumes private FROM fields and transfers selected routine identities.

use super::{
    QueryPlan, RoutineResolution, RowSchema, SQLError, SQLParam, SchemaScope, SourcePlan,
    SourceScope,
};
use crate::plan::{MutableSourcePlan, SourceChildSlot};

/// Bind the exact routine identities of a privately owned FROM tree and retain its construction leases.
pub fn bind_source_plan_for_execution_owned(
    routines: &dyn RoutineResolution,
    source: MutableSourcePlan,
    params: &[SQLParam],
    context: super::super::production::ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
    let subqueries = context.scalar_subqueries;
    SchemaScope::from_context_with_control(context, control)?
        .bind_owned_source_for_execution(routines, source, subqueries, params, outer)
}

impl SchemaScope {
    pub(in crate::binding) fn bind_owned_source_for_execution(
        &mut self,
        routines: &dyn RoutineResolution,
        source: MutableSourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
        self.bind_owned_source_in_scope(
            routines,
            source,
            subqueries,
            params,
            SourceScope::new(outer),
        )
    }

    fn bind_owned_source_in_scope(
        &mut self,
        routines: &dyn RoutineResolution,
        source: MutableSourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
        let (source, schema) =
            self.bind_owned_source_inner(routines, source, subqueries, params, scope)?;
        let schema = if scope.retain_names {
            SourceScope::describe_output_with_control(&source, schema, &self.control())?
        } else {
            schema
        };
        Ok((source, schema))
    }

    fn bind_owned_source_inner(
        &mut self,
        routines: &dyn RoutineResolution,
        mut source: MutableSourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
        let resources = self.production.clone();
        let control = resources.control();
        control.check()?;
        if matches!(&*source, SourcePlan::Join { .. }) {
            return self.bind_owned_join(routines, source, subqueries, params, scope);
        }
        let input = scope.input_with_control(&source, &control)?;
        let outer = input.as_ref();
        match &*source {
            SourcePlan::Function {
                name, relations, ..
            } => {
                let lower =
                    crate::semantics::builtin_function_dispatch_name_with_control(name, &control)?;
                if crate::registry::is_operator_join_table_function(&lower) {
                    super::super::catalog_sources::operator_join_relation_schemas_with_control(
                        &self.catalog,
                        &self.resolution,
                        relations.as_ref(),
                        &control,
                    )?;
                } else if let Some(binding) = self
                    .select_execution_table_function(routines, &source, subqueries, params, outer)?
                {
                    let (binding, memory) = binding.into_parts();
                    source = source.replace_function_binding(
                        None,
                        control.finish(Some(binding), memory)?,
                        &control,
                    )?;
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                let count = functions.len();
                for index in 0..count {
                    let SourcePlan::FunctionGroup { functions, .. } = &*source else {
                        unreachable!("original function group")
                    };
                    let member = functions[index].copy_for_execution(&control)?;
                    let (member, _) = self.bind_owned_source_for_execution(
                        routines, member, subqueries, params, outer,
                    )?;
                    let binding = member.into_function_binding(&control)?;
                    source = source.replace_function_binding(Some(index), binding, &control)?;
                }
            }
            SourcePlan::Join { .. } => unreachable!("join binding has its own namespace"),
            SourcePlan::RetainedRows(_)
            | SourcePlan::Table { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::Subquery { .. } => {}
        }
        let schema = self.bind_source(routines, &source, subqueries, params, outer)?;
        Ok((source, schema))
    }

    fn bind_owned_join(
        &mut self,
        routines: &dyn RoutineResolution,
        mut source: MutableSourcePlan,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        scope: SourceScope<'_>,
    ) -> Result<(MutableSourcePlan, RowSchema), SQLError> {
        let resources = self.production.clone();
        let control = resources.control();
        let mut left_schema = None;
        source = source.map_child(SourceChildSlot::Left, &control, |left| {
            let (left, schema) = self.bind_owned_source_in_scope(
                routines,
                left,
                subqueries,
                params,
                scope.with_names(),
            )?;
            left_schema = Some(schema);
            Ok(left)
        })?;
        let left_schema = left_schema.expect("bound join left input");
        let SourcePlan::Join {
            left,
            kind,
            lateral,
            ..
        } = &*source
        else {
            unreachable!("original join")
        };
        let lateral = *lateral;
        let preceding = scope.right_namespace_with_control(left, &left_schema, *kind, &control)?;
        let mut right_schema = None;
        source = source.map_child(SourceChildSlot::Right, &control, |right| {
            let (right, schema) = self.bind_owned_source_in_scope(
                routines,
                right,
                subqueries,
                params,
                SourceScope {
                    outer: scope.outer,
                    preceding: Some(&preceding),
                    inherited_lateral: lateral,
                    retain_names: true,
                },
            )?;
            right_schema = Some(schema);
            Ok(right)
        })?;
        let right_schema = right_schema.expect("bound join right input");
        let condition = scope.condition_with_control(&control)?;
        let SourcePlan::Join {
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            ..
        } = &*source
        else {
            unreachable!("original join")
        };
        let schema = self.bind_join_output_schema(super::JoinSchemaBinding {
            routines,
            kind: *kind,
            on: on.as_ref(),
            using: using.as_ref(),
            natural: *natural,
            alias: alias.as_deref(),
            column_aliases,
            left: &left_schema,
            right: &right_schema,
            subqueries,
            params,
            outer: condition.as_ref(),
        })?;
        Ok((source, schema))
    }
}
