//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare original dependency summaries with immutable references; no catalog entry is repaired.

use super::{invalid, visit_rule_relations, StoredRule};
use crate::{
    ast::FunctionBinding,
    binding::view_dependencies::visit_stored_plan_function_bindings,
    catalog::{
        resolution::{bound_relation_identity_with_control, RelationNameResolution},
        roles::RoleReference,
    },
    plan::{CtePlan, ExpressionPlan, PlanRef, UnifiedPlan},
    semantics::privileges::{
        columns::{self, ColumnDependencies},
        context::{PrivilegeCatalog, PrivilegeCteCatalog, PrivilegeScope},
    },
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{ProductionControl, ProductionMap},
    RelationIdentity,
};

pub fn validate(
    rule: &StoredRule,
    catalog: &dyn PrivilegeCatalog,
    resolution: &RelationNameResolution,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let dependencies = rule
        .dependencies
        .as_ref()
        .ok_or_else(|| invalid("missing rule dependencies"))?;
    let mut expected = References::new(dependencies, control)?;
    let inherited = EmptyCtes(&resolution.current_user);
    let scope = PrivilegeScope::borrowed_with_control(
        catalog,
        resolution.copy_with_control(control)?,
        &inherited,
        &[],
        control,
    );
    visit_rule_relations(rule, control, &mut |name, _| expected.relation(name))?;
    if let Some(expression) = &rule.definition.condition {
        let expression = ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?;
        expected.columns(columns::stored_expression(
            expression.scalar(),
            expression.queries(),
            &rule.definition.table,
            &scope,
        )?)?;
        expected.scalar(expression.scalar())?;
        for query in expression.queries() {
            expected.plan(PlanRef::Query(query))?;
        }
    }
    if let Some(condition) = &rule.condition_plan {
        expected.columns(columns::stored_expression(
            &condition.scalar,
            &condition.subqueries,
            &rule.definition.table,
            &scope,
        )?)?;
        expected.scalar(&condition.scalar)?;
        for query in &condition.subqueries {
            expected.plan(PlanRef::Query(query))?;
        }
        let binding = rule
            .condition_binding
            .as_ref()
            .ok_or_else(|| invalid("incomplete captured rule condition"))?;
        let owner = bound_relation_identity_with_control(&rule.definition.table, control)?;
        let mut captured = |root: &ScalarExpr| {
            root.try_visit(&mut |expression| {
                control.check()?;
                if let ScalarExpr::InternalColumn(column) = expression {
                    if let Some(name) = binding.column_name(*column) {
                        expected.column(&owner, name)?;
                    }
                }
                Ok::<_, SQLError>(true)
            })
        };
        captured(&condition.scalar)?;
        for query in &condition.subqueries {
            query.try_visit_scalar_expressions(&mut captured)?;
        }
    }
    for action in &rule.definition.actions {
        let plan = UnifiedPlan::lower_borrowed_for_rewrite(action, &|_: &str| false, control)?;
        expected.columns(columns::stored_plan(
            plan.as_plan_ref(),
            &rule.definition.table,
            &scope,
        )?)?;
        expected.plan(plan.as_plan_ref())?;
    }
    if expected.seen.len() != expected.count {
        return Err(invalid("rule dependencies contain unreferenced entries"));
    }
    control.check()?;
    Ok(())
}

struct EmptyCtes<'a>(&'a RoleReference);
impl PrivilegeCteCatalog for EmptyCtes<'_> {
    fn is_visible_cte(&self, _: &str) -> bool {
        false
    }
    fn materialized_columns(&self, _: &str) -> Option<Vec<String>> {
        None
    }
    fn deferred_reference(&self, _: &str) -> Option<&CtePlan> {
        None
    }
    fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        Ok(self.0)
    }
}

type RoutineKey<'a> = (Option<[u8; 16]>, &'a str, &'a [String]);
struct References<'a, 'control> {
    relations: ProductionMap<'control, &'a RelationIdentity, usize>,
    columns: ProductionMap<'control, (&'a RelationIdentity, &'a str), usize>,
    routines: ProductionMap<'control, RoutineKey<'a>, usize>,
    seen: ProductionMap<'control, usize, ()>,
    count: usize,
    control: ProductionControl<'control>,
}
impl<'a, 'control> References<'a, 'control> {
    fn new(
        dependencies: &'a crate::catalog::events::RuleDependencies,
        control: &ProductionControl<'control>,
    ) -> Result<Self, SQLError> {
        let mut output = Self {
            relations: ProductionMap::new(*control),
            columns: ProductionMap::new(*control),
            routines: ProductionMap::new(*control),
            seen: ProductionMap::new(*control),
            count: 0,
            control: *control,
        };
        for relation in &dependencies.relations {
            output.relations.insert_copy(relation, output.count)?;
            output.count += 1;
        }
        for column in &dependencies.columns {
            output
                .columns
                .insert_copy((&column.relation, column.column.as_str()), output.count)?;
            output.count += 1;
        }
        for routine in &dependencies.routines {
            output.routines.insert_copy(
                (
                    routine.object_id,
                    routine.name.as_str(),
                    routine.argument_types.as_slice(),
                ),
                output.count,
            )?;
            output.count += 1;
        }
        Ok(output)
    }
    fn mark(&mut self, index: Option<usize>, kind: &str) -> Result<(), SQLError> {
        let index = index.ok_or_else(|| {
            SQLError::Internal(format!(
                "invalid native event reference: rule dependencies omit a {kind}"
            ))
        })?;
        self.seen.insert_copy(index, ())?;
        Ok(())
    }
    fn relation(&mut self, name: &str) -> Result<(), SQLError> {
        let relation = bound_relation_identity_with_control(name, &self.control)?;
        if crate::catalog::VirtualRelation::at(&relation.schema, &relation.name).is_some() {
            return Ok(());
        }
        self.mark(self.relations.get(&&*relation).copied(), "relation")
    }
    fn column(&mut self, relation: &RelationIdentity, column: &str) -> Result<(), SQLError> {
        self.mark(self.columns.get(&(relation, column)).copied(), "column")
    }
    fn columns(&mut self, columns: ColumnDependencies) -> Result<(), SQLError> {
        for (name, columns) in columns {
            let relation = bound_relation_identity_with_control(&name, &self.control)?;
            for column in columns {
                self.column(&relation, &column)?;
            }
        }
        Ok(())
    }
    fn routine(&mut self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.control.check()?;
        if binding.builtin {
            return Ok(());
        }
        self.mark(
            self.routines
                .get(&(
                    binding.object_id,
                    binding.name.as_str(),
                    binding.argument_types.as_slice(),
                ))
                .copied(),
            "routine",
        )
    }
    fn scalar(&mut self, expression: &ScalarExpr) -> Result<(), SQLError> {
        expression.try_visit(&mut |node| {
            self.control.check()?;
            if let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = node
            {
                self.routine(binding)?;
            }
            Ok::<_, SQLError>(true)
        })
    }
    fn plan(&mut self, plan: PlanRef<'_>) -> Result<(), SQLError> {
        plan.try_visit_scalar_expressions(&mut |expression| self.scalar(expression))?;
        let control = self.control;
        visit_stored_plan_function_bindings(plan, &control, &mut |binding| self.routine(binding))
    }
}

#[cfg(test)]
mod tests;
