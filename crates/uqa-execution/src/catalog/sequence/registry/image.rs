//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pair native sequence records with their exact captured authority before publishing a catalog.

use super::SequenceCatalog;
use crate::catalog::sequence::image::{decode, invalid, MAGIC, RECORD_BYTES};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    image_io::ImageInput,
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    catalog::{
        security::sequence_registry::SequenceSecurities,
        temporary_namespace::is_temporary_schema_name,
    },
    SQLError,
};

impl SequenceCatalog {
    /// Reconstruct an unpublished catalog under one allowance. Record and authority images must have identical ordered membership and persistence. Role, table/column and database-wide OID closure remain the enclosing restore owner's responsibility.
    pub fn read_images(
        metadata: &mut dyn Read,
        authorities: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut input = ImageInput {
            reader: metadata,
            memory,
            cancellation,
        };
        if input.array::<8>()? != MAGIC {
            return Err(invalid("version header"));
        }
        let count = input.count()?;
        let security = SequenceSecurities::read_image(authorities, memory, cancellation)?;
        if security.len() != count {
            return Err(invalid("authority membership count"));
        }
        let mut result = Self::with_budget(memory);
        result.security = security;
        let mut oids = BudgetedMap::new(memory);
        for _ in 0..count {
            cancellation.check()?;
            let relation = relation(&mut input, result.records.last_durable_key())?;
            let record = decode(input.array::<RECORD_BYTES>()?)?;
            if result.security.persistence(&relation) != Some(record.persistence) {
                return Err(invalid("authority membership or persistence"));
            }
            if result.durable_objects.get(&record.object_id).is_some() {
                return Err(invalid("duplicate object identity"));
            }
            if let Some(oid) = record.catalog_oid {
                result.catalog_oids.insert(record.object_id, oid, 0)?;
            }
            if oids
                .insert(result.catalog_oid(&record.object_id), ())?
                .is_some()
            {
                return Err(invalid("duplicate sequence OID"));
            }
            result.durable_objects.insert(
                record.object_id,
                record.state.definition_generation,
                0,
            )?;
            result.records.insert_image_entry(relation, record)?;
        }
        input.end()?;
        Ok(result)
    }
}

fn compare(
    left: &str,
    right: &str,
    cancellation: &CancellationToken,
) -> Result<Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        cancellation.check()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    cancellation.check()?;
    Ok(left.len().cmp(&right.len()))
}

fn relation(
    input: &mut ImageInput<'_>,
    previous: Option<&RelationIdentity>,
) -> Result<Budgeted<RelationIdentity>, SQLError> {
    let schema = input.text()?;
    let name = input.text()?;
    if schema.is_empty()
        || name.is_empty()
        || schema.contains('\0')
        || name.contains('\0')
        || is_temporary_schema_name(&schema)
    {
        return Err(invalid("relation name"));
    }
    if let Some(previous) = previous {
        let mut order = compare(&previous.schema, &schema, input.cancellation)?;
        if order == Ordering::Equal {
            order = compare(&previous.name, &name, input.cancellation)?;
        }
        if order != Ordering::Less {
            return Err(invalid("record order"));
        }
    }
    let (schema, mut memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    memory.absorb(name_memory);
    Ok(Budgeted::new(RelationIdentity { schema, name }, memory))
}
