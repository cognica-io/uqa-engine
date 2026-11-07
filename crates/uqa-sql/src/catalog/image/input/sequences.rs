//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority images retain exact persistence and bound grants without resolving roles.

use super::{invalid, Input};
use crate::{
    ast::RelationPersistence,
    catalog::{
        security::{sequence_registry::SequenceSecurities, BoundSequenceSecurity},
        temporary_namespace::is_temporary_schema_name,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken, RelationIdentity,
};

pub(in super::super) fn sequence_authorities(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SequenceSecurities, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::SEQUENCE_AUTHORITIES)?;
    let mut result = SequenceSecurities::with_budget(memory);
    for _ in 0..count {
        cancellation.check()?;
        let schema = input.binary.text()?;
        let name = input.binary.text()?;
        if schema.is_empty()
            || name.is_empty()
            || schema.contains('\0')
            || name.contains('\0')
            || is_temporary_schema_name(&schema)
        {
            return Err(invalid("sequence relation name"));
        }
        if let Some(previous) = result.last_durable_key() {
            let mut order = input.compare_text(&previous.schema, &schema)?;
            if order == Ordering::Equal {
                order = input.compare_text(&previous.name, &name)?;
            }
            if order != Ordering::Less {
                return Err(invalid("sequence authority order"));
            }
        }
        let persistence = match input.binary.array::<1>()?[0] {
            0 => RelationPersistence::Permanent,
            1 => RelationPersistence::Unlogged,
            _ => return Err(invalid("sequence persistence")),
        };
        let role_owner = input.identity()?;
        let (acl, acl_memory) = input.acl()?.into_parts();
        let (schema, mut key_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        key_memory.absorb(name_memory);
        result.insert_image_entry(
            Budgeted::new(RelationIdentity { schema, name }, key_memory),
            persistence,
            Budgeted::new(BoundSequenceSecurity { role_owner, acl }, acl_memory),
        )?;
    }
    input.binary.end()?;
    Ok(result)
}
