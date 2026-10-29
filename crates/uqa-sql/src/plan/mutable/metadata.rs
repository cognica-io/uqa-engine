//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nonallocating plan decisions and independently replaceable projection metadata.

use super::{
    memory::RewriteMemory, MutableQueryPlan, OwnedScalarExpr, QueryScalarSlot, RelationalPlan,
};
use crate::{
    plan::{AccessPathPlan, ComputePlan},
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{BudgetedVec, OwnedSet, Produced, ProductionControl},
    Value,
};

impl MutableQueryPlan {
    pub fn append_row_lock(
        mut self,
        strength: crate::ast::LockStrength,
        wait: crate::ast::LockWait,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Ok(self);
        };
        let clause = crate::ast::LockingClause {
            strength,
            wait,
            relations: Vec::new(),
        };
        if let Some(mut copying) = super::Copying::for_rewrite(*control) {
            let count = block.locking.len() + 1;
            let locking = copying.map(0..count, |copying, index| {
                let clause = block.locking.get(index).unwrap_or(&clause);
                Ok(crate::ast::LockingClause {
                    strength: clause.strength,
                    wait: clause.wait,
                    relations: copying.texts(&clause.relations)?,
                })
            })?;
            block.locking = locking;
            *block_memory(self.memory.as_mut().expect("controlled query")).field_mut("locking") =
                Some(copying.into_rewrite_memory());
        } else {
            block.locking.push(clause);
        }
        Ok(self)
    }

    pub fn set_privilege_subject(
        mut self,
        subject: Option<Produced<crate::catalog::roles::RoleReference>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (subject, owner) = if let Some(mut copying) = super::Copying::for_rewrite(*control) {
            let subject = subject
                .map(|subject| copying.take_privilege_subject(subject))
                .transpose()?;
            (subject, Some(copying.into_rewrite_memory()))
        } else {
            (
                subject.map(|subject| {
                    subject
                        .into_uncontrolled()
                        .expect("ordinary privilege subject")
                }),
                None,
            )
        };
        self.query.relation_privilege_subject = subject;
        if let Some(memory) = &mut self.memory {
            *memory.field_mut("relation_privilege_subject") = owner;
        }
        Ok(self)
    }

    pub fn clear_privilege_columns(
        mut self,
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
        if let Some(memory) = &mut self.memory {
            let owner = memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut("block")
                .as_mut()
                .expect("query block")
                .field_mut("privilege_columns");
            *owner = super::Copying::for_rewrite(*control).map(super::Copying::into_rewrite_memory);
        }
        Ok(self)
    }

    /// Retain one privilege dependency after admitting its tree node. Duplicate identities retain the original node and release the unused supplied identity.
    pub fn include_privilege_column(
        mut self,
        identity: Produced<crate::ColumnIdentity>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "column privilege dependency requires a query block".into(),
            ));
        };
        if block.privilege_columns.contains(&*identity) {
            return Ok(self);
        }
        let node = control.reserve(OwnedSet::<crate::ColumnIdentity>::entry_bytes())?;
        let (identity, payload) = identity.into_parts();
        let memory = control.combine(payload, node);
        block.privilege_columns.insert(identity);
        if let Some(owner) = &mut self.memory {
            let RewriteMemory::Fields { direct, .. } = block_memory(owner)
                .field_mut("privilege_columns")
                .as_mut()
                .expect("privilege column owner")
            else {
                unreachable!("query block field owner");
            };
            direct.absorb(memory.expect("controlled privilege dependency"));
        }
        Ok(self)
    }

    /// Change only the allocation-free compute decision chosen by the planner.
    #[expect(
        clippy::needless_match,
        reason = "exhaustive inline reconstruction prevents future payload-bearing variants from bypassing admission"
    )]
    pub fn set_compute(
        mut self,
        compute: ComputePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let compute = match compute {
            ComputePlan::Project => ComputePlan::Project,
            ComputePlan::Aggregate => ComputePlan::Aggregate,
            ComputePlan::Window => ComputePlan::Window,
        };
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "compute decision requires a query block".into(),
            ));
        };
        block.compute = compute;
        Ok(self)
    }

    /// Change only the allocation-free access decision chosen by the planner.
    pub fn set_access(
        mut self,
        access: AccessPathPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let access = match access {
            AccessPathPlan::Row => AccessPathPlan::Row,
            AccessPathPlan::Hybrid => AccessPathPlan::Hybrid,
            AccessPathPlan::OperatorTree {
                score_limit_pushdown,
            } => AccessPathPlan::OperatorTree {
                score_limit_pushdown,
            },
        };
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "access decision requires a query block".into(),
            ));
        };
        block.access = access;
        Ok(self)
    }

    /// Select one expression and destroy every other query allocation, including its former projection and parent containers. An absent optional field returns None.
    pub fn select_scalar(
        self,
        slot: QueryScalarSlot,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        let mut selected = None;
        let query = self.map_scalar(slot, control, |expression| {
            selected = Some(expression);
            Ok(ScalarExpr::Literal(Value::Null).copy_for_rewrite(control)?)
        })?;
        drop(query);
        control.check()?;
        Ok(selected)
    }

    /// Replace a projection's name without changing its scalar or positional descriptor. The new text arrives from an admitted producer; removed text is freed before its lease.
    pub fn replace_projection_alias(
        mut self,
        index: usize,
        alias: Option<Produced<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "projection alias requires a query block".into(),
            ));
        };
        let (alias, reservation) = match alias {
            Some(alias) => {
                let (alias, memory) = alias.into_parts();
                (Some(alias), memory)
            }
            None => (None, control.empty_reservation()),
        };
        // Construct the replacement owner before replacing any old payload, including if a mismatched allowance panics.
        let replacement = control.finish(alias, reservation)?;
        let (alias, reservation) = replacement.into_parts();
        let memory = reservation.map(|direct| RewriteMemory::Fields {
            fields: BudgetedVec::new(direct.budget()),
            direct,
        });
        block.projections[index].alias = alias;
        if let Some(owner) = &mut self.memory {
            *projection_memory(owner, index).field_mut("alias") = memory;
        }
        Ok(self)
    }
}

fn projection_memory(memory: &mut RewriteMemory, index: usize) -> &mut RewriteMemory {
    block_memory(memory)
        .field_mut("projections")
        .as_mut()
        .expect("projection vector")
        .item_mut(index)
        .as_mut()
        .expect("projection item")
}

fn block_memory(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut("block")
        .as_mut()
        .expect("query block")
}
