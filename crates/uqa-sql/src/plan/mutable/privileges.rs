//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the column and scalar-query authority of discarded projections.

use super::{metadata::block_memory, MutableQueryPlan, RewriteMemory};
use crate::{
    plan::{QueryBlockPlan, RelationalPlan},
    ColumnIdentity, SQLError, ScalarExpr,
};
use uqa_core::memory::{
    MemoryReservation, OwnedSet, Produced, ProductionControl, ProductionMap, ProductionVec,
};

struct PrivilegeMemory {
    columns: Option<MemoryReservation>,
    subqueries: Option<MemoryReservation>,
}

impl QueryBlockPlan {
    /// Whether replacing this query boundary must preserve recorded column or scalar-query access.
    pub fn has_projection_privileges(&self) -> bool {
        !self.privilege_columns.is_empty() || !self.privilege_subqueries.is_empty()
    }

    /// Return the local slots retained solely for column and table permissions. A slot also referenced by an executable expression still participates in typing and preparation.
    pub fn privilege_only_subquery_slots<'c>(
        &self,
        control: ProductionControl<'c>,
    ) -> Result<ProductionMap<'c, usize, ()>, SQLError> {
        let mut slots = ProductionMap::new(control);
        for &slot in &self.privilege_subqueries {
            slots.insert_copy(slot, ())?;
        }
        if slots.is_empty() {
            return Ok(slots);
        }
        self.try_visit_expressions(&mut |expression| {
            let finished = expression.try_any_node(|node| -> Result<bool, SQLError> {
                control.check()?;
                if let Some(slot) = crate::semantics::subquery_reference(node) {
                    slots.remove(&slot);
                }
                Ok(slots.is_empty())
            })?;
            Ok::<_, SQLError>(!finished)
        })?;
        Ok(slots)
    }

    /// Record column and scalar-query references before replacing caller-owned projection expressions.
    pub fn preserve_projection_privileges(
        &mut self,
        positions: impl IntoIterator<Item = usize>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.record_projection_privileges(positions, control)?;
        Ok(())
    }

    fn record_projection_privileges(
        &mut self,
        positions: impl IntoIterator<Item = usize>,
        control: &ProductionControl<'_>,
    ) -> Result<PrivilegeMemory, SQLError> {
        let mut subqueries = ProductionMap::new(*control);
        let mut added = OwnedSet::<ColumnIdentity>::new();
        let mut memory = control.empty_reservation();
        for position in positions {
            let projection = self.projections.get(position).ok_or_else(|| {
                SQLError::Internal("projection privilege index out of range".into())
            })?;
            projection.expr.try_visit(&mut |part| {
                control.check()?;
                let (qualifier, column) = match part {
                    ScalarExpr::Column(column) => (None, column.as_str()),
                    ScalarExpr::QualifiedColumn { qualifier, column } => {
                        (Some(qualifier.as_str()), column.as_str())
                    }
                    ScalarExpr::ScalarSubquery(slot)
                    | ScalarExpr::Exists { subquery: slot, .. }
                    | ScalarExpr::InSubquery { subquery: slot, .. } => {
                        if *slot >= self.subqueries.len() {
                            return Err(SQLError::Internal(format!(
                                "missing projection subquery slot {slot}"
                            )));
                        }
                        if self.privilege_subqueries.binary_search(slot).is_err() {
                            subqueries.insert_copy(*slot, ())?;
                        }
                        return Ok(true);
                    }
                    _ => return Ok(true),
                };
                let compare = |stored: &ColumnIdentity| {
                    (qualifier, column).cmp(&(stored.qualifier(), stored.column()))
                };
                if !self.privilege_columns.contains_by(compare) && !added.contains_by(compare) {
                    let identity =
                        ColumnIdentity::from_parts_with_control(qualifier, column, control)?;
                    let node = insert_column(&mut added, identity, control)?;
                    memory = control.combine(memory.take(), node);
                }
                Ok::<_, SQLError>(true)
            })?;
        }
        let slots = if subqueries.is_empty() {
            None
        } else {
            for slot in &self.privilege_subqueries {
                subqueries.insert_copy(*slot, ())?;
            }
            let mut slots = ProductionVec::new(*control);
            slots.reserve(subqueries.len())?;
            for (slot, ()) in subqueries {
                slots.push_copy(slot)?;
            }
            Some(slots.finish()?)
        };
        control.check()?;
        self.privilege_columns.append(added);
        let subqueries = slots.and_then(|slots| {
            let (slots, lease) = slots.into_parts();
            self.privilege_subqueries = slots;
            lease
        });
        Ok(PrivilegeMemory {
            columns: memory,
            subqueries,
        })
    }
}

fn insert_column(
    columns: &mut OwnedSet<ColumnIdentity>,
    identity: Produced<ColumnIdentity>,
    control: &ProductionControl<'_>,
) -> Result<Option<MemoryReservation>, SQLError> {
    let node = control.reserve(OwnedSet::<ColumnIdentity>::entry_bytes())?;
    let (identity, payload) = identity.into_parts();
    columns.insert(identity);
    Ok(control.combine(payload, node))
}

impl MutableQueryPlan {
    /// Preserve the dependencies of discarded projections and transfer their new nodes to the query's existing privilege owner.
    pub fn preserve_projection_privileges(
        mut self,
        positions: impl IntoIterator<Item = usize>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "projection privileges require a query block".into(),
            ));
        };
        let previous = block.privilege_subqueries.capacity() * size_of::<usize>();
        let added = block.record_projection_privileges(positions, control)?;
        if let Some(owner) = self.memory.as_mut() {
            let direct = projection_privilege_memory(owner);
            direct.absorb(added.columns.expect("controlled privilege columns"));
            if let Some(slots) = added.subqueries {
                drop(direct.split(previous));
                direct.absorb(slots);
            }
        }
        Ok(self)
    }

    /// Retain one privilege dependency after admitting its tree node. Duplicate identities retain the original node and release the unused supplied identity.
    pub fn include_privilege_column(
        mut self,
        identity: Produced<ColumnIdentity>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "column privilege dependency requires a query block".into(),
            ));
        };
        if !block.privilege_columns.contains(&*identity) {
            let memory = insert_column(&mut block.privilege_columns, identity, control)?;
            if let Some(owner) = self.memory.as_mut() {
                projection_privilege_memory(owner)
                    .absorb(memory.expect("controlled privilege column"));
            }
        }
        Ok(self)
    }

    pub fn clear_privilege_columns(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.clear_privileges(false, control)
    }

    pub(crate) fn clear_projection_privileges(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.clear_privileges(true, control)
    }

    fn clear_privileges(
        mut self,
        clear_subqueries: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "privilege column reset requires a query block".into(),
            ));
        };
        block.privilege_columns.clear();
        if clear_subqueries {
            block.privilege_subqueries = Vec::new();
        }
        if let Some(owner) = self.memory.as_mut() {
            let direct = projection_privilege_memory(owner);
            // The column nodes have been freed. Keep just the reservation for the surviving slot buffer.
            *direct = direct.split(block.privilege_subqueries.capacity() * size_of::<usize>());
        }
        Ok(self)
    }
}

// A block owns its recorded column nodes and scalar slots in one constructor field. Both allocations are empty on initial lowering.
fn projection_privilege_memory(owner: &mut RewriteMemory) -> &mut MemoryReservation {
    let RewriteMemory::Fields { direct, .. } = block_memory(owner)
        .field_mut("projection_privileges")
        .as_mut()
        .expect("projection privilege owner")
    else {
        unreachable!("query block field owner")
    };
    direct
}
