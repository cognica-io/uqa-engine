//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{empty_source, Node, Retire};
use crate::plan::{
    CommandPlan, DeletePlan, InsertPlan, MergePlan, UnifiedPlan, UpdatePlan, ViewRuleReturningPlan,
};

pub(super) fn retire_scalars(command: &mut CommandPlan) {
    match command {
        CommandPlan::Empty => {}
        CommandPlan::Insert(value) => value.retire(),
        CommandPlan::Update(value) => value.retire(),
        CommandPlan::Delete(value) => value.retire(),
        CommandPlan::Merge(value) => value.retire(),
        _ => {}
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
    fn retire(&mut self) {
        self.columns.retire();
        self.rows.retire();
        self.on_conflict.retire();
        self.returning.retire();
        self.view_checks.retire();
        if let Some(rule) = &mut self.view_rule_returning {
            rule.returning.retire();
        }
    }
}

impl Retire for UpdatePlan {
    fn retire(&mut self) {
        self.assignments.retire();
        self.predicate.retire();
        self.returning.retire();
        self.view_checks.retire();
        if let Some(rule) = &mut self.view_rule_returning {
            rule.returning.retire();
        }
    }
}

impl Retire for DeletePlan {
    fn retire(&mut self) {
        self.predicate.retire();
        self.returning.retire();
        if let Some(rule) = &mut self.view_rule_returning {
            rule.returning.retire();
        }
    }
}

impl Retire for MergePlan {
    fn retire(&mut self) {
        self.target_predicate.retire();
        self.join_condition.retire();
        self.when_clauses.retire();
        self.returning.retire();
        self.view_checks.retire();
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
