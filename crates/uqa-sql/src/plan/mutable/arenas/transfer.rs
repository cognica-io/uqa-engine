//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Moving exclusive scalar-query children preserves their constructor owners and local namespaces.

use super::super::{Copying, MutableQueryPlan, OwnedScalarExpr, QueryChildSlot, RewriteMemory};
use crate::{
    plan::{QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl MutableQueryPlan {
    /// Transfer the detached expression's exclusive query children into another query's arena. Invalid or shared references leave all three private inputs unchanged. Successful movement preserves each child query's own namespace and compacts only the source root's indices.
    pub fn transfer_expression_to(
        mut self,
        mut destination: Self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Self, OwnedScalarExpr, bool), SQLError> {
        self.assert_owner(control);
        destination.assert_owner(control);
        control.check()?;
        let expression = expression.rewrite(control, Ok::<_, SQLError>)?;
        let mut selected = ProductionMap::new(*control);
        expression.try_visit(&mut |node| {
            control.check()?;
            if let Some(id) = query_id(node) {
                selected.insert_copy(id, ())?;
            }
            Ok::<_, SQLError>(true)
        })?;
        if selected.is_empty() {
            return Ok((self, destination, expression, true));
        }
        let width = arena(&mut self.query.root).len();
        if selected.iter().any(|(id, ())| *id >= width) {
            return Ok((self, destination, expression, false));
        }
        let mut exclusive = true;
        let mut checked = Ok(());
        crate::plan::subqueries::visit_root(&mut self.query.root, &mut |node| {
            if checked.is_ok() {
                checked = control.check();
                if let Some(id) = query_id(node) {
                    exclusive &= id < width && selected.get(&id).is_none();
                }
            }
        });
        checked?;
        if !exclusive {
            return Ok((self, destination, expression, false));
        }
        let previous = arena(&mut destination.query.root).len();
        let mut moved = ProductionMap::new(*control);
        let mut retained = ProductionMap::new(*control);
        for id in 0..width {
            if selected.get(&id).is_some() {
                moved.insert_copy(id, previous + moved.len())?;
            } else {
                retained.insert_copy(id, retained.len())?;
            }
        }
        let mut selected_ids = selected.iter();
        let mut take = |index| {
            if index < previous {
                take_query(&mut destination, index)
            } else {
                let (id, ()) = selected_ids.next().expect("selected child count");
                take_query(&mut self, *id)
            }
        };
        let count = previous + selected.len();
        let (children, memory) = match Copying::for_rewrite(*control) {
            Some(mut copying) => {
                let children =
                    copying.map(0..count, |copying, index| copying.take_query(take(index)))?;
                (children, Some(copying.into_rewrite_memory()))
            }
            None => ((0..count).map(|index| take(index).query).collect(), None),
        };
        *arena(&mut destination.query.root) = children;
        if let Some(owner) = &mut destination.memory {
            *arena_memory(owner) = memory;
        }
        compact(&mut self, &selected);
        let expression = expression.remap_subquery_ids(control, &mut |id| {
            *moved.get(&id).expect("selected expression query")
        })?;
        let mut checked = Ok(());
        crate::plan::subqueries::visit_root(&mut self.query.root, &mut |node| {
            if checked.is_ok() {
                checked = control.check();
                if let Some(id) = query_id_mut(node) {
                    *id = *retained.get(id).expect("retained expression query");
                }
            }
        });
        checked?;
        Ok((self, destination, expression, true))
    }
}

fn take_query(query: &mut MutableQueryPlan, index: usize) -> MutableQueryPlan {
    let slot = QueryChildSlot::Subquery(index);
    MutableQueryPlan {
        query: std::mem::replace(
            slot.query_mut(&mut query.query),
            super::super::queries::empty_query(),
        ),
        memory: query
            .memory
            .as_mut()
            .and_then(|memory| slot.memory_mut(memory).take()),
    }
}

fn compact(query: &mut MutableQueryPlan, selected: &ProductionMap<'_, usize, ()>) {
    let arena = arena(&mut query.query.root);
    let mut fields = query.memory.as_mut().map(|memory| {
        let RewriteMemory::Fields { fields, .. } =
            arena_memory(memory).as_mut().expect("query arena")
        else {
            unreachable!("query arena fields")
        };
        fields
    });
    let mut retained = 0;
    for index in 0..arena.len() {
        if selected.get(&index).is_none() {
            arena.swap(retained, index);
            if let Some(fields) = &mut fields {
                fields.swap(retained, index);
            }
            retained += 1;
        }
    }
    arena.truncate(retained);
    if let Some(fields) = fields {
        fields.truncate(retained);
    }
}

fn arena(root: &mut RelationalPlan) -> &mut Vec<QueryPlan> {
    match root {
        RelationalPlan::QueryBlock(block) => &mut block.subqueries,
        RelationalPlan::Values { subqueries, .. } | RelationalPlan::SetOp { subqueries, .. } => {
            subqueries
        }
    }
}

fn arena_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    let root = memory.field_mut("root").as_mut().expect("query root");
    let RewriteMemory::Fields { fields, .. } = root else {
        unreachable!("query root fields")
    };
    let has_block = fields.iter().any(|field| field.name == "block");
    let root = if has_block {
        root.field_mut("block").as_mut().expect("query block")
    } else {
        root
    };
    root.field_mut("subqueries")
}

fn query_id(node: &ScalarExpr) -> Option<usize> {
    match node {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => Some(*id),
        _ => None,
    }
}

fn query_id_mut(node: &mut ScalarExpr) -> Option<&mut usize> {
    match node {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => Some(id),
        _ => None,
    }
}
