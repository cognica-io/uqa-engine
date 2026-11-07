//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, MemoryReservation, SQLError};
use crate::{
    ast::ReturningAliases,
    plan::{CommandPlan, DeletePlan, InsertPlan, MergePlan, SourcePlan, UpdatePlan},
    ScalarExpr,
};

pub(super) fn aliases() -> ReturningAliases {
    ReturningAliases {
        old: String::new(),
        new: String::new(),
        old_explicit: false,
        new_explicit: false,
    }
}

pub(super) fn read(
    input: &mut Input<'_>,
    memory: &mut MemoryReservation,
) -> Result<CommandPlan, SQLError> {
    Ok(match input.binary.array::<1>()?[0] {
        0 => {
            memory.grow(size_of::<InsertPlan>())?;
            CommandPlan::Insert(Box::new(InsertPlan {
                table: String::new(),
                target_relation_bound: false,
                relations_bound: false,
                statement_privilege_subject: None,
                target_privilege_subject: None,
                target_qualifier: String::new(),
                target_alias: None,
                include_descendants: false,
                columns: Vec::new(),
                overriding: None,
                ctes: Vec::new(),
                rows: Vec::new(),
                source: None,
                rule_values_input: None,
                on_conflict: None,
                returning: Vec::new(),
                returning_aliases: aliases(),
                subqueries: Vec::new(),
                view_checks: Vec::new(),
                view_rule_relations: Vec::new(),
                view_rule_insert_plans: Vec::new(),
                view_rule_returning: None,
            }))
        }
        1 => {
            memory.grow(size_of::<UpdatePlan>())?;
            CommandPlan::Update(Box::new(UpdatePlan {
                table: String::new(),
                target_relation_bound: false,
                relations_bound: false,
                statement_privilege_subject: None,
                target_privilege_subject: None,
                target_qualifier: String::new(),
                target_alias: None,
                include_descendants: false,
                assignments: Vec::new(),
                predicate: None,
                ctes: Vec::new(),
                source: None,
                returning: Vec::new(),
                returning_aliases: aliases(),
                subqueries: Vec::new(),
                view_checks: Vec::new(),
                view_rule_relations: Vec::new(),
                view_rule_update_plans: Vec::new(),
                view_rule_returning: None,
            }))
        }
        2 => {
            memory.grow(size_of::<DeletePlan>())?;
            CommandPlan::Delete(Box::new(DeletePlan {
                table: String::new(),
                target_relation_bound: false,
                relations_bound: false,
                statement_privilege_subject: None,
                target_privilege_subject: None,
                target_qualifier: String::new(),
                target_alias: None,
                include_descendants: false,
                predicate: None,
                ctes: Vec::new(),
                source: None,
                returning: Vec::new(),
                returning_aliases: aliases(),
                subqueries: Vec::new(),
                view_rule_relations: Vec::new(),
                view_rule_returning: None,
            }))
        }
        3 => {
            memory.grow(size_of::<MergePlan>() + size_of::<SourcePlan>())?;
            CommandPlan::Merge(Box::new(MergePlan {
                ctes: Vec::new(),
                target: String::new(),
                statement_privilege_subject: None,
                target_privilege_subject: None,
                target_qualifier: String::new(),
                target_alias: None,
                include_descendants: false,
                target_predicate: None,
                source: Box::new(super::super::empty_source()),
                join_condition: ScalarExpr::Star,
                when_clauses: Vec::new(),
                returning: Vec::new(),
                returning_aliases: aliases(),
                subqueries: Vec::new(),
                view_checks: Vec::new(),
            }))
        }
        _ => return Err(super::super::invalid("query mutation constructor")),
    })
}
