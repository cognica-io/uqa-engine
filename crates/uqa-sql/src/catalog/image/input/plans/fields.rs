//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive native plan fields transfer their payload leases without replay or resolution.

use super::super::{expressions::payload, scalars::References};
use super::{
    invalid,
    retirement::{Pending, Retire},
    Input, MemoryReservation, SQLError, ScalarExpr,
};
use crate::catalog::roles::{identity::RoleBinding, RoleIdentity, RoleReference};
use uqa_core::memory::{BudgetedVec, OwnedSet};

impl Input<'_> {
    pub(super) fn plan_text(&mut self, memory: &mut MemoryReservation) -> Result<String, SQLError> {
        Ok(payload(self.binary.text()?, memory))
    }

    pub(super) fn plan_optional_text(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<String>, SQLError> {
        Ok(payload(self.optional_text()?, memory))
    }

    pub(super) fn plan_vector<T>(
        &mut self,
        memory: &mut MemoryReservation,
        mut read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<T, SQLError>,
    ) -> Result<Vec<T>, SQLError> {
        let count = self.binary.count()?;
        self.items(count, memory, |input, memory| read(input, memory))
    }

    pub(super) fn plan_scalar_vector<T: Retire>(
        &mut self,
        memory: &mut MemoryReservation,
        mut read: impl FnMut(&mut Self, &mut MemoryReservation) -> Result<Pending<T>, SQLError>,
    ) -> Result<Pending<Vec<T>>, SQLError> {
        let count = self.binary.count()?;
        let mut buffer = BudgetedVec::new(self.binary.memory);
        buffer.reserve(count)?;
        let (buffer, reservation) = buffer.into_parts();
        memory.absorb(reservation);
        let mut values = Pending::new(buffer);
        for _ in 0..count {
            self.binary.cancellation.check()?;
            values.push(read(self, memory)?.take());
        }
        Ok(values)
    }

    pub(super) fn plan_scalar(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<ScalarExpr>, SQLError> {
        let (value, reservation, observed) = self.native_scalar()?.into_native_parts();
        memory.absorb(reservation);
        references.absorb(observed);
        Ok(Pending::new(value))
    }

    pub(super) fn plan_optional_scalar(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<Option<ScalarExpr>>, SQLError> {
        Ok(Pending::new(if self.flag()? {
            Some(self.plan_scalar(memory, references)?.take())
        } else {
            None
        }))
    }

    pub(super) fn plan_optional_scalar_box(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<Option<Box<ScalarExpr>>>, SQLError> {
        if !self.flag()? {
            return Ok(Pending::new(None));
        }
        memory.grow(size_of::<ScalarExpr>())?;
        Ok(Pending::new(Some(Box::new(
            self.plan_scalar(memory, references)?.take(),
        ))))
    }

    pub(super) fn plan_scalars(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<Vec<ScalarExpr>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            input.plan_scalar(memory, references)
        })
    }

    pub(super) fn plan_rows(
        &mut self,
        memory: &mut MemoryReservation,
        references: &mut References,
    ) -> Result<Pending<Vec<Vec<ScalarExpr>>>, SQLError> {
        self.plan_scalar_vector(memory, |input, memory| {
            input.plan_scalars(memory, references)
        })
    }

    pub(super) fn plan_role(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Option<RoleReference>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        match self.binary.array::<1>()?[0] {
            0 => Ok(Some(RoleReference::Named(self.plan_text(memory)?))),
            1 => {
                let name = self.binary.text()?;
                let oid = u32::from_le_bytes(self.binary.array()?);
                let object_id = self.binary.array()?;
                if name.is_empty() {
                    return Err(invalid("empty query role name"));
                }
                super::super::validate_identity(RoleIdentity {
                    oid: i64::from(oid),
                    object_id,
                })?;
                Ok(Some(RoleReference::Bound(RoleBinding::from_captured_name(
                    name, oid, object_id,
                )?)))
            }
            _ => Err(invalid("query role constructor")),
        }
    }

    pub(super) fn plan_positions(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<OwnedSet<usize>, SQLError> {
        let count = self.binary.count()?;
        let mut result = OwnedSet::new();
        let mut previous = None;
        for _ in 0..count {
            let value = self.binary.count()?;
            if previous.is_some_and(|previous| previous >= value) {
                return Err(invalid("query position order"));
            }
            memory.grow(OwnedSet::<usize>::entry_bytes())?;
            result.insert(value);
            previous = Some(value);
        }
        Ok(result)
    }
}
