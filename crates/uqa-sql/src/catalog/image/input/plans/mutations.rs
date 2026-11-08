//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{driver::Step, Input, Kind, Node, PlanMemory, SQLError};
use crate::plan::{CommandPlan, QueryPlan, SourcePlan};

mod attachment;
mod constructors;
mod fields;

#[expect(
    clippy::too_many_lines,
    reason = "four native mutation schemas retain their original ordered fields and separate rewrite arenas"
)]
pub(super) fn read(
    command: &mut CommandPlan,
    position: usize,
    count: &mut Option<usize>,
    references: &mut References,
    rule_references: &mut References,
    input: &mut Input<'_>,
    memory: &mut PlanMemory,
) -> Result<Step, SQLError> {
    if position == 0 {
        *command = constructors::read(input, memory)?;
        return Ok(Step::Advance);
    }
    match command {
        CommandPlan::Insert(value) => match position {
            1 => value.table = input.plan_text(memory)?,
            2 => value.target_relation_bound = input.flag()?,
            3 => value.relations_bound = input.flag()?,
            4 => value.statement_privilege_subject = input.plan_role(memory)?,
            5 => value.target_privilege_subject = input.plan_role(memory)?,
            6 => value.target_qualifier = input.plan_text(memory)?,
            7 => value.target_alias = input.plan_optional_text(memory)?,
            8 => value.include_descendants = input.flag()?,
            9 => value.columns = input.plan_targets(memory, references)?.take(),
            10 => value.overriding = input.plan_overriding()?,
            11 => return input.plan_children(memory, count, &mut value.ctes, Kind::Cte),
            12 => value.rows = input.plan_rows(memory, references)?.take(),
            13 => return input.plan_optional_box::<QueryPlan>(memory, Kind::Query),
            14 => value.rule_values_input = input.plan_rule_values(memory)?,
            15 => value.on_conflict = input.plan_conflict(memory, references)?.take(),
            16 => value.returning = input.plan_projections(memory, references)?.take(),
            17 => value.returning_aliases = input.plan_returning_aliases(memory)?,
            18 => return input.plan_children(memory, count, &mut value.subqueries, Kind::Query),
            19 => value.view_checks = input.plan_view_checks(memory, references)?.take(),
            20 => value.view_rule_relations = input.plan_texts(memory)?,
            21 => value.view_rule_insert_plans = input.plan_rule_inserts(memory)?,
            22 => {
                return fields::returning(
                    &mut value.view_rule_returning,
                    count,
                    rule_references,
                    input,
                    memory,
                )
            }
            _ => return Ok(Step::Done),
        },
        CommandPlan::Update(value) => match position {
            1 => value.table = input.plan_text(memory)?,
            2 => value.target_relation_bound = input.flag()?,
            3 => value.relations_bound = input.flag()?,
            4 => value.statement_privilege_subject = input.plan_role(memory)?,
            5 => value.target_privilege_subject = input.plan_role(memory)?,
            6 => value.target_qualifier = input.plan_text(memory)?,
            7 => value.target_alias = input.plan_optional_text(memory)?,
            8 => value.include_descendants = input.flag()?,
            9 => value.assignments = input.plan_assignments(memory, references)?.take(),
            10 => value.predicate = input.plan_optional_scalar(memory, references)?.take(),
            11 => return input.plan_children(memory, count, &mut value.ctes, Kind::Cte),
            12 => return input.plan_optional_box::<SourcePlan>(memory, Kind::Source),
            13 => value.returning = input.plan_projections(memory, references)?.take(),
            14 => value.returning_aliases = input.plan_returning_aliases(memory)?,
            15 => return input.plan_children(memory, count, &mut value.subqueries, Kind::Query),
            16 => value.view_checks = input.plan_view_checks(memory, references)?.take(),
            17 => value.view_rule_relations = input.plan_texts(memory)?,
            18 => value.view_rule_update_plans = input.plan_rule_updates(memory)?,
            19 => {
                return fields::returning(
                    &mut value.view_rule_returning,
                    count,
                    rule_references,
                    input,
                    memory,
                )
            }
            _ => return Ok(Step::Done),
        },
        CommandPlan::Delete(value) => match position {
            1 => value.table = input.plan_text(memory)?,
            2 => value.target_relation_bound = input.flag()?,
            3 => value.relations_bound = input.flag()?,
            4 => value.statement_privilege_subject = input.plan_role(memory)?,
            5 => value.target_privilege_subject = input.plan_role(memory)?,
            6 => value.target_qualifier = input.plan_text(memory)?,
            7 => value.target_alias = input.plan_optional_text(memory)?,
            8 => value.include_descendants = input.flag()?,
            9 => value.predicate = input.plan_optional_scalar(memory, references)?.take(),
            10 => return input.plan_children(memory, count, &mut value.ctes, Kind::Cte),
            11 => return input.plan_optional_box::<SourcePlan>(memory, Kind::Source),
            12 => value.returning = input.plan_projections(memory, references)?.take(),
            13 => value.returning_aliases = input.plan_returning_aliases(memory)?,
            14 => return input.plan_children(memory, count, &mut value.subqueries, Kind::Query),
            15 => value.view_rule_relations = input.plan_texts(memory)?,
            16 => {
                return fields::returning(
                    &mut value.view_rule_returning,
                    count,
                    rule_references,
                    input,
                    memory,
                )
            }
            _ => return Ok(Step::Done),
        },
        CommandPlan::Merge(value) => match position {
            1 => return input.plan_children(memory, count, &mut value.ctes, Kind::Cte),
            2 => value.target = input.plan_text(memory)?,
            3 => value.statement_privilege_subject = input.plan_role(memory)?,
            4 => value.target_privilege_subject = input.plan_role(memory)?,
            5 => value.target_qualifier = input.plan_text(memory)?,
            6 => value.target_alias = input.plan_optional_text(memory)?,
            7 => value.include_descendants = input.flag()?,
            8 => value.target_predicate = input.plan_optional_scalar(memory, references)?.take(),
            9 => return Ok(Step::Child(Kind::Source)),
            10 => value.join_condition = input.plan_scalar(memory, references)?.take(),
            11 => value.when_clauses = input.plan_when_clauses(memory, references)?.take(),
            12 => value.returning = input.plan_projections(memory, references)?.take(),
            13 => value.returning_aliases = input.plan_returning_aliases(memory)?,
            14 => return input.plan_children(memory, count, &mut value.subqueries, Kind::Query),
            15 => value.view_checks = input.plan_view_checks(memory, references)?.take(),
            _ => return Ok(Step::Done),
        },
        _ => unreachable!("query mutation constructor"),
    }
    Ok(Step::Advance)
}

pub(super) use attachment::attach;
