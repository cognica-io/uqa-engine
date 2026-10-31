//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query children transfer independently from their enclosing boxes and arenas.

use super::{memory::RewriteMemory, MutableQueryPlan, QueryPlan, RelationalPlan};
use crate::{plan::CtePlanBody, SQLError};
use uqa_core::memory::ProductionControl;

/// A directly retained query child. Each child keeps its own scalar-subquery namespace.
#[derive(Clone, Copy, Debug)]
pub enum QueryChildSlot {
    Cte(usize),
    Left,
    Right,
    Subquery(usize),
}

impl MutableQueryPlan {
    /// Rewrite a query child while retaining its original parent box or arena slot. A failed callback destroys the complete private parent and all untouched siblings.
    pub fn map_query_child(
        mut self,
        slot: QueryChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = Self {
            query: std::mem::replace(slot.query_mut(&mut self.query), empty_query()),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| slot.memory_mut(memory).take()),
        };
        let output = map(input)?;
        output.assert_owner(control);
        control.check()?;
        *slot.query_mut(&mut self.query) = output.query;
        if let Some(memory) = &mut self.memory {
            *slot.memory_mut(memory) = output.memory;
        }
        Ok(self)
    }
}

impl QueryChildSlot {
    pub(super) fn query_mut(self, query: &mut QueryPlan) -> &mut QueryPlan {
        match self {
            Self::Cte(index) => match &mut query.ctes[index].body {
                CtePlanBody::Query(query) => query,
                CtePlanBody::Command(_) => panic!("query child selector requires a query CTE"),
            },
            Self::Left | Self::Right => match &mut query.root {
                RelationalPlan::SetOp { left, right, .. } => {
                    if matches!(self, Self::Left) {
                        left
                    } else {
                        right
                    }
                }
                _ => panic!("set child selector requires a set-operation root"),
            },
            Self::Subquery(index) => match &mut query.root {
                RelationalPlan::QueryBlock(block) => &mut block.subqueries[index],
                RelationalPlan::SetOp { subqueries, .. }
                | RelationalPlan::Values { subqueries, .. } => &mut subqueries[index],
            },
        }
    }

    pub(super) fn memory_mut(self, memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
        let input = match self {
            Self::Cte(index) => memory
                .field_mut("ctes")
                .as_mut()
                .expect("CTE vector")
                .item_mut(index)
                .as_mut()
                .expect("CTE item")
                .field_mut("body")
                .as_mut()
                .expect("CTE body"),
            Self::Left | Self::Right => memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut(if matches!(self, Self::Left) {
                    "left"
                } else {
                    "right"
                })
                .as_mut()
                .expect("set input"),
            Self::Subquery(index) => {
                let root = memory.field_mut("root").as_mut().expect("query root");
                let block = match root {
                    RewriteMemory::Fields { fields, .. } => {
                        fields.iter().any(|field| field.name == "block")
                    }
                    RewriteMemory::Scalar(_) => unreachable!("query root owner"),
                };
                let root = if block {
                    root.field_mut("block").as_mut().expect("query block")
                } else {
                    root
                };
                root.field_mut("subqueries")
                    .as_mut()
                    .expect("query arena")
                    .item_mut(index)
                    .as_mut()
                    .expect("query arena item")
            }
        };
        input.field_mut("query")
    }
}

pub(super) fn empty_query() -> QueryPlan {
    QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relations_bound: false,
        relation_privilege_subject: None,
        ctes: Vec::new(),
        root: RelationalPlan::Values {
            rows: Vec::new(),
            subqueries: Vec::new(),
        },
    }
}
