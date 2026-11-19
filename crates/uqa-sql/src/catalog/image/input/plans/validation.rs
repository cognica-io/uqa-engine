//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::scalars::References;
use super::{invalid, Frame, Input, Node, SQLError};
use crate::plan::{CommandPlan, RelationalPlan};

impl References {
    fn validate(self, queries: usize, windows: usize) -> Result<(), SQLError> {
        if self.query.is_some_and(|index| index >= queries) {
            return Err(invalid("native query arena address"));
        }
        if self.window.is_some_and(|index| index >= windows) {
            return Err(invalid("native window arena address"));
        }
        Ok(())
    }
}

impl Frame {
    pub(super) fn validate(&self, input: &Input<'_>) -> Result<(), SQLError> {
        let mut position = 0;
        loop {
            input.binary.cancellation.check()?;
            if self.validate_step(position)? {
                return Ok(());
            }
            position += 1;
        }
    }

    pub(super) fn validate_step(&self, position: usize) -> Result<bool, SQLError> {
        match &self.node {
            Node::Query(value) => match &value.root {
                RelationalPlan::QueryBlock(block) => {
                    self.references
                        .validate(block.subqueries.len(), block.windows.len())?;
                    if let Some(window) = block.windows.get(position) {
                        if window.spec.definition.is_some() {
                            return Err(invalid("canonical window call address"));
                        }
                        if let Some(parent) = window.inherited {
                            if parent >= position || block.windows[parent].name.is_none() {
                                return Err(invalid("native window inheritance address"));
                            }
                        }
                        Ok(false)
                    } else {
                        Ok(true)
                    }
                }
                RelationalPlan::SetOp { subqueries, .. }
                | RelationalPlan::Values { subqueries, .. } => {
                    self.references.validate(subqueries.len(), 0).map(|()| true)
                }
            },
            Node::Expression(value) => self
                .references
                .validate(value.subqueries.len(), 0)
                .map(|()| true),
            Node::Cte(_) => self.references.validate(0, 0).map(|()| true),
            Node::Source(_) => Ok(true), // Direct sources borrow the enclosing query or mutation arena; attachment transfers their observed addresses.
            Node::Command(value) => {
                let (queries, rule) = match value {
                    CommandPlan::Insert(value) => {
                        (&value.subqueries, value.view_rule_returning.as_ref())
                    }
                    CommandPlan::Update(value) => {
                        (&value.subqueries, value.view_rule_returning.as_ref())
                    }
                    CommandPlan::Delete(value) => {
                        (&value.subqueries, value.view_rule_returning.as_ref())
                    }
                    CommandPlan::Merge(value) => (&value.subqueries, None),
                    _ => unreachable!("native mutation input"),
                };
                self.references.validate(queries.len(), 0)?;
                self.rule_references
                    .validate(rule.map_or(0, |value| value.subqueries.len()), 0)
                    .map(|()| true)
            }
            Node::Empty => unreachable!("completed native plan"),
        }
    }
}
