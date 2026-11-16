//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{empty_source, Node, Retire};
use crate::plan::{CommandPlan, ViewRuleReturningPlan};

pub(super) fn retire_scalars(command: &mut CommandPlan) {
    match command {
        CommandPlan::Empty => {}
        CommandPlan::Insert(value) => {
            value.columns.retire();
            value.rows.retire();
            value.on_conflict.retire();
            value.returning.retire();
            value.view_checks.retire();
            if let Some(rule) = &mut value.view_rule_returning {
                rule.returning.retire();
            }
        }
        CommandPlan::Update(value) => {
            value.assignments.retire();
            value.predicate.retire();
            value.returning.retire();
            value.view_checks.retire();
            if let Some(rule) = &mut value.view_rule_returning {
                rule.returning.retire();
            }
        }
        CommandPlan::Delete(value) => {
            value.predicate.retire();
            value.returning.retire();
            if let Some(rule) = &mut value.view_rule_returning {
                rule.returning.retire();
            }
        }
        CommandPlan::Merge(value) => {
            value.target_predicate.retire();
            value.join_condition.retire();
            value.when_clauses.retire();
            value.returning.retire();
            value.view_checks.retire();
        }
        _ => unreachable!("query input admits only mutation commands"),
    }
}

fn rule_child(value: &mut Option<ViewRuleReturningPlan>) -> Option<Node> {
    value.as_mut()?.subqueries.pop().map(Node::Query)
}

pub(super) fn child(command: &mut CommandPlan, position: &mut usize) -> Option<Node> {
    match command {
        CommandPlan::Empty => None,
        CommandPlan::Insert(value) => {
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
        CommandPlan::Update(value) => {
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
        CommandPlan::Delete(value) => {
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
        CommandPlan::Merge(value) => {
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
        _ => unreachable!("query input admits only mutation commands"),
    }
}
