//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CommandPlan, Node};

pub(in super::super) fn attach(command: &mut CommandPlan, position: &mut usize, child: Node) {
    match (command, child) {
        (CommandPlan::Insert(value), Node::Cte(cte)) => value.ctes.push(cte),
        (CommandPlan::Update(value), Node::Cte(cte)) => value.ctes.push(cte),
        (CommandPlan::Delete(value), Node::Cte(cte)) => value.ctes.push(cte),
        (CommandPlan::Merge(value), Node::Cte(cte)) => value.ctes.push(cte),
        (CommandPlan::Insert(value), Node::Query(query)) => match *position {
            13 => {
                value.source = Some(Box::new(query));
                *position += 1;
            }
            18 => value.subqueries.push(query),
            22 => value
                .view_rule_returning
                .as_mut()
                .expect("rule returning input")
                .subqueries
                .push(query),
            _ => unreachable!("INSERT query slot"),
        },
        (CommandPlan::Update(value), Node::Source(source)) => {
            value.source = Some(Box::new(source));
            *position += 1;
        }
        (CommandPlan::Delete(value), Node::Source(source)) => {
            value.source = Some(Box::new(source));
            *position += 1;
        }
        (CommandPlan::Merge(value), Node::Source(source)) => {
            *value.source = source;
            *position += 1;
        }
        (CommandPlan::Update(value), Node::Query(query)) => match *position {
            15 => value.subqueries.push(query),
            19 => value
                .view_rule_returning
                .as_mut()
                .expect("rule returning input")
                .subqueries
                .push(query),
            _ => unreachable!("UPDATE query slot"),
        },
        (CommandPlan::Delete(value), Node::Query(query)) => match *position {
            14 => value.subqueries.push(query),
            16 => value
                .view_rule_returning
                .as_mut()
                .expect("rule returning input")
                .subqueries
                .push(query),
            _ => unreachable!("DELETE query slot"),
        },
        (CommandPlan::Merge(value), Node::Query(query)) => value.subqueries.push(query),
        _ => unreachable!("mutation child constructor"),
    }
}
