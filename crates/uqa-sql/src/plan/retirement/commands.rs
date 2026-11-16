//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{empty_source, Node, Retire};
use crate::plan::{
    CommandPlan, DeletePlan, InsertPlan, MergePlan, UnifiedPlan, UpdatePlan, ViewRuleReturningPlan,
};

pub(super) fn retire_scalars(
    command: &mut CommandPlan,
    positions: &mut [usize],
    scalar: &mut Option<crate::ir::ownership::Retirement>,
) -> bool {
    match command {
        CommandPlan::Insert(value) => value.retire_step(positions, scalar),
        CommandPlan::Update(value) => value.retire_step(positions, scalar),
        CommandPlan::Delete(value) => value.retire_step(positions, scalar),
        CommandPlan::Merge(value) => value.retire_step(positions, scalar),
        _ => true,
    }
}

fn rule_child(value: &mut Option<ViewRuleReturningPlan>) -> Option<Node> {
    value.as_mut()?.subqueries.pop().map(Node::Query)
}

pub(super) fn child(command: &mut CommandPlan, position: &mut usize) -> Option<Node> {
    match command {
        CommandPlan::Empty => None,
        CommandPlan::Insert(value) => insert_child(value),
        CommandPlan::Update(value) => update_child(value),
        CommandPlan::Delete(value) => delete_child(value),
        CommandPlan::Merge(value) => merge_child(value, position),
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. }
            if *position == 0 =>
        {
            *position = 1;
            Some(Node::Query(std::mem::replace(
                query.as_mut(),
                super::empty_query(),
            )))
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } if *position == 0 => {
            *position = 1;
            Some(match body.as_mut() {
                UnifiedPlan::Query(query) => {
                    Node::Query(std::mem::replace(query.as_mut(), super::empty_query()))
                }
                UnifiedPlan::Command(command) => {
                    Node::Command(std::mem::replace(command.as_mut(), CommandPlan::Empty))
                }
            })
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            params.pop().map(Node::Expression)
        }
        _ => None,
    }
}

impl Retire for InsertPlan {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        super::scalars::fields!(positions, scalar; self.columns, self.rows, self.on_conflict, self.returning, self.view_checks)
            && self
                .view_rule_returning
                .as_mut()
                .is_none_or(|rule| rule.returning.retire_step(positions, scalar))
    }
}
impl Retire for UpdatePlan {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        super::scalars::fields!(positions, scalar; self.assignments, self.predicate, self.returning, self.view_checks)
            && self
                .view_rule_returning
                .as_mut()
                .is_none_or(|rule| rule.returning.retire_step(positions, scalar))
    }
}
impl Retire for DeletePlan {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        super::scalars::fields!(positions, scalar; self.predicate, self.returning)
            && self
                .view_rule_returning
                .as_mut()
                .is_none_or(|rule| rule.returning.retire_step(positions, scalar))
    }
}
impl Retire for MergePlan {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        super::scalars::fields!(positions, scalar; self.target_predicate, self.join_condition, self.when_clauses, self.returning, self.view_checks)
    }
}

pub(super) fn insert_child(value: &mut InsertPlan) -> Option<Node> {
    if let Some(cte) = value.ctes.pop() {
        return Some(Node::Cte(cte));
    }
    if let Some(query) = value.source.take() {
        return Some(Node::Query(*query));
    }
    value
        .subqueries
        .pop()
        .map(Node::Query)
        .or_else(|| rule_child(&mut value.view_rule_returning))
}

pub(super) fn update_child(value: &mut UpdatePlan) -> Option<Node> {
    if let Some(cte) = value.ctes.pop() {
        return Some(Node::Cte(cte));
    }
    if let Some(source) = value.source.take() {
        return Some(Node::Source(*source));
    }
    value
        .subqueries
        .pop()
        .map(Node::Query)
        .or_else(|| rule_child(&mut value.view_rule_returning))
}

pub(super) fn delete_child(value: &mut DeletePlan) -> Option<Node> {
    if let Some(cte) = value.ctes.pop() {
        return Some(Node::Cte(cte));
    }
    if let Some(source) = value.source.take() {
        return Some(Node::Source(*source));
    }
    value
        .subqueries
        .pop()
        .map(Node::Query)
        .or_else(|| rule_child(&mut value.view_rule_returning))
}

pub(super) fn merge_child(value: &mut MergePlan, position: &mut usize) -> Option<Node> {
    if let Some(cte) = value.ctes.pop() {
        return Some(Node::Cte(cte));
    }
    if *position == 0 {
        *position = 1;
        return Some(Node::Source(std::mem::replace(
            value.source.as_mut(),
            empty_source(),
        )));
    }
    value.subqueries.pop().map(Node::Query)
}
