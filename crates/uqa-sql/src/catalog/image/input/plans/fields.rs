//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive native plan fields transfer their payload leases without replay or resolution.

use super::super::{expressions::payload, scalars::References};
use super::{invalid, retirement::Pending, Fields, Input, PlanMemory, SQLError, ScalarExpr};
use crate::catalog::roles::{identity::RoleBinding, RoleIdentity, RoleReference};
use uqa_core::memory::OwnedSet;

macro_rules! input_fields {
    ($input:ident, $read:ident $(, $async:ident)?) => {
        impl $input<'_> {
            pub(super) $($async)? fn plan_text(&mut self, memory: &mut PlanMemory) -> Result<String, SQLError> {
                Ok(payload($read!(self.binary.text())?, memory))
            }

            pub(super) $($async)? fn plan_optional_text(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<Option<String>, SQLError> {
                Ok(payload($read!(self.optional_text())?, memory))
            }

            pub(super) $($async)? fn plan_scalar(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<ScalarExpr>, SQLError> {
                let (value, reservation, observed) =
                    $read!(self.native_scalar())?.into_native_parts();
                memory.absorb(reservation);
                references.absorb(observed);
                Ok(Pending::new(value))
            }

            pub(super) $($async)? fn plan_optional_scalar(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Option<ScalarExpr>>, SQLError> {
                Ok(Pending::new(if $read!(self.flag())? {
                    Some($read!(self.plan_scalar(memory, references))?.take())
                } else {
                    None
                }))
            }

            pub(super) $($async)? fn plan_optional_scalar_box(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Option<Box<ScalarExpr>>>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(Pending::new(None));
                }
                memory.grow(size_of::<ScalarExpr>())?;
                Ok(Pending::new(Some(Box::new(
                    $read!(self.plan_scalar(memory, references))?.take(),
                ))))
            }

            pub(super) $($async)? fn plan_scalars(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<ScalarExpr>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    { $read!(input.plan_scalar(memory, references)) }
                })
            }

            pub(super) $($async)? fn plan_rows(
                &mut self,
                memory: &mut PlanMemory,
                references: &mut References,
            ) -> Result<Pending<Vec<Vec<ScalarExpr>>>, SQLError> {
                plan_scalar_list!($read, self, memory, |input, memory| {
                    { $read!(input.plan_scalars(memory, references)) }
                })
            }

            pub(super) $($async)? fn plan_role(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<Option<RoleReference>, SQLError> {
                if !$read!(self.flag())? {
                    return Ok(None);
                }
                match $read!(self.binary.array::<1>())?[0] {
                    0 => Ok(Some(RoleReference::Named($read!(
                        self.plan_text(memory)
                    )?))),
                    1 => {
                        let name = $read!(self.binary.text())?;
                        let oid = u32::from_le_bytes($read!(self.binary.array())?);
                        let object_id = $read!(self.binary.array())?;
                        if name.is_empty() {
                            return Err(invalid("empty query role name"));
                        }
                        super::super::validate_identity(RoleIdentity {
                            oid: i64::from(oid),
                            object_id,
                        })?;
                        let role = RoleBinding::from_captured_name(name, oid, object_id)?;
                        memory.retain_child(role.reserved_bytes())?;
                        Ok(Some(RoleReference::Bound(role)))
                    }
                    _ => Err(invalid("query role constructor")),
                }
            }

            pub(super) $($async)? fn plan_positions(
                &mut self,
                memory: &mut PlanMemory,
            ) -> Result<OwnedSet<usize>, SQLError> {
                let count = $read!(self.binary.count())?;
                let mut result = OwnedSet::new();
                let mut previous = None;
                for _ in 0..count {
                    let value = $read!(self.binary.count())?;
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
    };
}

input_fields!(Input, read_field);
input_fields!(Fields, await_field, async);
