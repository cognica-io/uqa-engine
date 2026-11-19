//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve routine parameters in mutation operands before assignment analysis. A stored MERGE keeps the expressions of removed write targets as dependencies.

use super::{
    AssignmentPlan, AssignmentTarget, CommandPlan, ConflictActionPlan, LiteralInput, Preparation,
    QueryPlan, RowSchema, SQLError, ScalarExpr, SchemaContext,
};
use crate::binding::overlay_outer_schema_with_control;

impl<I: LiteralInput, C: SchemaContext> Preparation<'_, I, C> {
    pub(super) fn observe_mutation_parameter_references(
        &mut self,
        command: &CommandPlan,
        target: &RowSchema,
        source: Option<&RowSchema>,
        input: &RowSchema,
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let subqueries = command.scalar_subqueries();
        match command {
            CommandPlan::Insert(insert) => {
                let empty = RowSchema::default();
                let values = outer.unwrap_or(&empty);
                if let Some(query) = &insert.source {
                    self.query(query, outer)?;
                }
                self.operand_references(
                    insert
                        .columns
                        .iter()
                        .flat_map(AssignmentTarget::expressions)
                        .chain(insert.rows.iter().flatten()),
                    values,
                    subqueries,
                )?;
                if let Some(conflict) = &insert.on_conflict {
                    self.operand_references(
                        conflict
                            .expressions
                            .iter()
                            .chain(conflict.predicate.as_deref()),
                        input,
                        subqueries,
                    )?;
                    if let ConflictActionPlan::Update { .. } = &conflict.action {
                        let update = self.conflict_update_scope(target, input)?;
                        self.operand_references(
                            insert.conflict_update_expressions(),
                            &update,
                            subqueries,
                        )?;
                    }
                }
            }
            CommandPlan::Update(update) => {
                self.operand_references(
                    update
                        .assignments
                        .iter()
                        .flat_map(AssignmentPlan::expressions)
                        .chain(update.predicate.as_ref()),
                    input,
                    subqueries,
                )?;
            }
            CommandPlan::Delete(delete) => {
                self.operand_references(delete.predicate.iter(), input, subqueries)?;
            }
            CommandPlan::Merge(merge) => {
                self.operand_references(
                    std::iter::once(&merge.join_condition).chain(merge.target_predicate.as_ref()),
                    input,
                    subqueries,
                )?;
                let qualifier = merge
                    .target_alias
                    .as_deref()
                    .unwrap_or(&merge.target_qualifier);
                let target = RowSchema::with_relation_qualifier_with_control(
                    target,
                    qualifier,
                    &self.scope.control(),
                )?;
                let target =
                    overlay_outer_schema_with_control(&target, outer, &self.scope.control())?;
                let source = overlay_outer_schema_with_control(
                    source.expect("MERGE source"),
                    outer,
                    &self.scope.control(),
                )?;
                for clause in &merge.when_clauses {
                    self.operand_references(
                        clause.expressions(),
                        clause.input_scope(input, &target, &source),
                        subqueries,
                    )?;
                }
            }
            _ => unreachable!("a mutation target was selected"),
        }
        Ok(())
    }

    fn operand_references<'a>(
        &mut self,
        expressions: impl IntoIterator<Item = &'a ScalarExpr>,
        scope: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        use crate::ir::{
            children::Scope,
            observation::{self, Decision},
        };
        let owner = self.scope.control().capture();
        let control = owner.control();
        for expression in expressions {
            crate::catalog::work::inline(observation::visit_in(
                expression,
                &control,
                &mut (),
                Scope::Complete,
                &mut |node| {
                    if let Some(position) = self.scope.routine_parameter_reference(node, scope) {
                        self.parameters.named_reference(node, position)?;
                        return Ok(Decision::Prune);
                    }
                    if let ScalarExpr::ScalarSubquery(id)
                    | ScalarExpr::Exists { subquery: id, .. }
                    | ScalarExpr::InSubquery { subquery: id, .. } = node
                    {
                        self.scalar_query_schema(*id, scope, subqueries)?;
                    }
                    Ok(Decision::Descend)
                },
            ))?;
        }
        Ok(())
    }
}
