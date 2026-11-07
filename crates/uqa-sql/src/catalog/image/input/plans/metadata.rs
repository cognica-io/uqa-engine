//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local metadata retains the original wire order and admitted scalar owners.

use super::super::{expressions::payload, scalars::References};
use super::{invalid, retirement::Pending, Input, PlanMemory, SQLError};
use crate::{
    ast::{
        CteMaterialization, FrameExclusion, FrameMode, FunctionBinding, JoinKind, JoinUsing,
        LockStrength, LockWait, LockingClause, NullsOrder, OperatorJoinRelations, OverridingKind,
        ReturningAliases, SetOpKind,
    },
    plan::{
        AccessPathPlan, ComputePlan, JoinExecutionStrategy, OrderPlan, ProjectionPlan,
        TableFunctionPlan,
    },
    ColumnIdentity, ColumnType,
};
use std::cmp::Ordering;
use uqa_core::memory::{OwnedSet, ProductionControl};

macro_rules! enum_field {
    ($method:ident, $ty:ident, $($tag:literal => $variant:ident),+ $(,)?) => {
        impl Input<'_> {
            pub(super) fn $method(&mut self) -> Result<$ty, SQLError> {
                match self.binary.array::<1>()?[0] {
                    $($tag => Ok($ty::$variant),)+
                    _ => Err(invalid(stringify!($ty))),
                }
            }
        }
    };
}
enum_field!(plan_nulls, NullsOrder, 0 => First, 1 => Last);
enum_field!(plan_set_kind, SetOpKind, 0 => Union, 1 => Intersect, 2 => Except);
enum_field!(plan_materialization, CteMaterialization, 0 => Default, 1 => Materialized, 2 => NotMaterialized);
enum_field!(plan_join_kind, JoinKind, 0 => Inner, 1 => Left, 2 => Right, 3 => Full, 4 => Cross);
enum_field!(plan_frame_mode, FrameMode, 0 => Rows, 1 => Range, 2 => Groups);
enum_field!(plan_exclusion, FrameExclusion, 0 => NoOthers, 1 => CurrentRow, 2 => Group, 3 => Ties);
enum_field!(plan_lock_strength, LockStrength, 0 => ForKeyShare, 1 => ForShare, 2 => ForNoKeyUpdate, 3 => ForUpdate);
enum_field!(plan_lock_wait, LockWait, 0 => Block, 1 => SkipLocked, 2 => NoWait);
enum_field!(plan_overriding_kind, OverridingKind, 0 => SystemValue, 1 => UserValue);
enum_field!(plan_compute, ComputePlan, 0 => Project, 1 => Aggregate, 2 => Window);
enum_field!(plan_strategy, JoinExecutionStrategy, 0 => Auto, 1 => Hash);

impl Input<'_> {
    pub(super) fn plan_texts(&mut self, memory: &mut PlanMemory) -> Result<Vec<String>, SQLError> {
        self.plan_vector(memory, Self::plan_text)
    }

    pub(super) fn plan_optional_count(&mut self) -> Result<Option<usize>, SQLError> {
        self.flag()?
            .then(|| self.binary.count().map_err(Into::into))
            .transpose()
    }

    pub(super) fn plan_optional_texts(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<Vec<String>>, SQLError> {
        self.flag()?.then(|| self.plan_texts(memory)).transpose()
    }

    pub(super) fn plan_binding(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<FunctionBinding>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        if self.header(super::super::super::FUNCTION_BINDING)? != 1 {
            return Err(invalid("query function binding count"));
        }
        Ok(Some(payload(self.function_binding()?, memory)))
    }

    pub(super) fn plan_optional_type(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.flag()?
            .then(|| self.column_type().map(|value| payload(value, memory)))
            .transpose()
    }

    pub(super) fn plan_overriding(&mut self) -> Result<Option<OverridingKind>, SQLError> {
        self.flag()?
            .then(|| self.plan_overriding_kind())
            .transpose()
    }

    pub(super) fn plan_optional_nulls(&mut self) -> Result<Option<NullsOrder>, SQLError> {
        self.flag()?.then(|| self.plan_nulls()).transpose()
    }

    pub(super) fn plan_relations(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<OperatorJoinRelations>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        Ok(Some(OperatorJoinRelations {
            left: self.plan_text(memory)?,
            right: self.plan_text(memory)?,
        }))
    }

    pub(super) fn plan_using(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Option<JoinUsing>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        Ok(Some(JoinUsing {
            columns: self.plan_texts(memory)?,
            alias: self.plan_optional_text(memory)?,
        }))
    }

    pub(super) fn plan_returning_aliases(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<ReturningAliases, SQLError> {
        Ok(ReturningAliases {
            old: self.plan_text(memory)?,
            new: self.plan_text(memory)?,
            old_explicit: self.flag()?,
            new_explicit: self.flag()?,
        })
    }

    pub(super) fn plan_access(&mut self) -> Result<AccessPathPlan, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(AccessPathPlan::Row),
            1 => Ok(AccessPathPlan::OperatorTree {
                score_limit_pushdown: self.flag()?,
            }),
            2 => Ok(AccessPathPlan::Hybrid),
            _ => Err(invalid("query access path")),
        }
    }

    pub(super) fn plan_locking(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<Vec<LockingClause>, SQLError> {
        self.plan_vector(memory, |input, memory| {
            Ok(LockingClause {
                strength: input.plan_lock_strength()?,
                wait: input.plan_lock_wait()?,
                relations: input.plan_texts(memory)?,
            })
        })
    }

    pub(super) fn plan_projections(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<ProjectionPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            let expr = input.plan_scalar(memory, references)?;
            let alias = input.plan_optional_text(memory)?;
            Ok(Pending::new(ProjectionPlan {
                expr: expr.take(),
                alias,
            }))
        })
    }

    pub(super) fn plan_orders(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<OrderPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            let expr = input.plan_scalar(memory, references)?;
            let descending = input.flag()?;
            let nulls = input.plan_optional_nulls()?;
            Ok(Pending::new(OrderPlan {
                expr: expr.take(),
                descending,
                nulls,
            }))
        })
    }

    pub(super) fn plan_functions(
        &mut self,
        memory: &mut PlanMemory,
        references: &mut References,
    ) -> Result<Pending<Vec<TableFunctionPlan>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            let name = input.plan_text(memory)?;
            let binding = input.plan_binding(memory)?;
            let output_name = input.plan_text(memory)?;
            let relations = input.plan_relations(memory)?;
            let args = input.plan_scalars(memory, references)?;
            let column_aliases = input.plan_texts(memory)?;
            let column_types = input.plan_texts(memory)?;
            Ok(Pending::new(TableFunctionPlan {
                name,
                binding,
                output_name,
                relations,
                args: args.take(),
                column_aliases,
                column_types,
            }))
        })
    }

    fn plan_column(&mut self, memory: &mut PlanMemory) -> Result<ColumnIdentity, SQLError> {
        let qualifier = self.optional_text()?;
        let column = self.binary.text()?;
        let control = ProductionControl::new(
            self.binary.memory,
            self.binary.cancellation,
            self.binary.cancellation,
        );
        let (value, lease) =
            ColumnIdentity::from_parts_with_control(qualifier.as_deref(), &column, &control)?
                .into_parts();
        memory.absorb(lease.expect("controlled column identity"));
        Ok(value)
    }

    fn plan_column_order(
        &self,
        left: &ColumnIdentity,
        right: &ColumnIdentity,
    ) -> Result<Ordering, SQLError> {
        let qualifier = match (left.qualifier(), right.qualifier()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left), Some(right)) => self.compare_text(left, right)?,
        };
        if qualifier == Ordering::Equal {
            self.compare_text(left.column(), right.column())
        } else {
            Ok(qualifier)
        }
    }

    pub(super) fn plan_columns(
        &mut self,
        memory: &mut PlanMemory,
    ) -> Result<OwnedSet<ColumnIdentity>, SQLError> {
        let count = self.binary.count()?;
        let mut result = OwnedSet::new();
        let mut previous = None;
        for _ in 0..count {
            memory.grow(OwnedSet::<ColumnIdentity>::entry_bytes())?;
            let value = self.plan_column(memory)?;
            if let Some(prior) = &previous {
                if self.plan_column_order(prior, &value)? != Ordering::Less {
                    return Err(invalid("query column order"));
                }
            }
            if let Some(prior) = previous.replace(value) {
                result.insert(prior);
            }
        }
        if let Some(last) = previous {
            result.insert(last);
        }
        Ok(result)
    }
}
