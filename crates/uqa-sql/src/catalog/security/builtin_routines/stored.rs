//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native stored fields retain their original buffers; record versions and catalog semantics belong to restoration.

use super::{BuiltinRoutineSecurities, BuiltinRoutineSecurity};
use crate::{
    ast::stored_input::{number, Input, InputError, Parts},
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{Budgeted, Produced, ProductionControl},
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSecurity {
    pub builtin_routine_acl_format: u32,
    pub entry: BuiltinRoutineSecurity,
}

impl StoredSecurity {
    /// Decode one physically bounded record using the original direct-struct JSON rules. Format and complete ACL validation follow at the physical catalog boundary.
    pub fn from_stored_json_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut input = Input::new(text, *control);
        let first = input.next()?;
        let mut parts = Parts::new(*control);
        let (mut format, mut entry) = (0, None);
        input.closed_fields(
            first,
            &["builtin_routine_acl_format", "entry"],
            [3, 3],
            |input, field, token| {
                if field == 0 {
                    format = number(token)?;
                } else {
                    entry = Some(parts.take(input.stored_builtin_security(token)?));
                }
                Ok(())
            },
        )?;
        input.end()?;
        Ok(parts.finish(Self {
            builtin_routine_acl_format: format,
            entry: entry.expect("required stored entry"),
        })?)
    }
}

impl<'a> Input<'a, '_> {
    fn stored_builtin_security(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<BuiltinRoutineSecurity>, InputError> {
        let mut parts = Parts::new(self.control);
        let (mut revision, mut execute_acl) = ([0; 16], Vec::new());
        self.closed_fields(
            first,
            &["revision", "execute_acl"],
            [3, 3],
            |input, field, token| {
                if field == 0 {
                    revision = input.identity(token)?;
                } else {
                    execute_acl = parts.take(input.array(token, Self::stored_object_acl)?);
                }
                Ok(())
            },
        )?;
        parts.finish(BuiltinRoutineSecurity {
            revision,
            execute_acl,
        })
    }
}

/// Transfer complete decoded buffers into the existing immutable registry without copying ACL paths.
pub fn insert_stored_entry(
    registry: &mut BuiltinRoutineSecurities,
    oid: u32,
    entry: Budgeted<BuiltinRoutineSecurity>,
) -> Result<(), SQLError> {
    let key = Budgeted::new(oid, registry.budget().empty_reservation());
    registry.insert_image_entry(key, entry)
}

#[cfg(test)]
mod tests;
