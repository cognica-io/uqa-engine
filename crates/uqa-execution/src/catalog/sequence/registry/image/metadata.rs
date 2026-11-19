//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sequence records retain incomplete relation fields and previously claimed identities.

use super::{invalid, SequenceCatalog};
use crate::catalog::sequence::image::{decode, MAGIC, RECORD_BYTES};
use std::cmp::Ordering;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecoder},
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{catalog::temporary_namespace::is_temporary_schema_name, SQLError};

pub(super) fn start(
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ImageDecoder<SequenceCatalog, SQLError>, SQLError> {
    Ok(ImageDecoder::new(
        memory,
        cancellation,
        |mut reader| async move { read(reader.input()).await },
    )?)
}

pub(super) async fn read(mut input: AsyncImageInput<'_>) -> Result<SequenceCatalog, SQLError> {
    if input.array::<8>().await? != MAGIC {
        return Err(invalid("version header"));
    }
    let count = input.count().await?;
    let mut result = SequenceCatalog::with_budget(input.memory);
    let mut oids = BudgetedMap::new(input.memory);
    for _ in 0..count {
        input.checkpoint().await?;
        let relation = relation(&mut input, result.records.last_durable_key()).await?;
        let record = decode(input.array::<RECORD_BYTES>().await?)?;
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
        result
            .durable_objects
            .insert(record.object_id, record.state.definition_generation, 0)?;
        result.records.insert_image_entry(relation, record)?;
    }
    input.end().await?;
    Ok(result)
}

async fn compare(
    left: &str,
    right: &str,
    input: &mut AsyncImageInput<'_>,
) -> Result<Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        input.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    input.cancellation.check()?;
    Ok(left.len().cmp(&right.len()))
}

async fn relation(
    input: &mut AsyncImageInput<'_>,
    previous: Option<&RelationIdentity>,
) -> Result<Budgeted<RelationIdentity>, SQLError> {
    let schema = input.text().await?;
    let name = input.text().await?;
    if schema.is_empty()
        || name.is_empty()
        || schema.contains('\0')
        || name.contains('\0')
        || is_temporary_schema_name(&schema)
    {
        return Err(invalid("relation name"));
    }
    if let Some(previous) = previous {
        let mut order = compare(&previous.schema, &schema, input).await?;
        if order == Ordering::Equal {
            order = compare(&previous.name, &name, input).await?;
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
