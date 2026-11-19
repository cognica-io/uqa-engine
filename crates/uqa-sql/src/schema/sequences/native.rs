//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sequence dependencies retain original table/column identities and bound names.

use crate::{
    ast::ColumnDef,
    catalog::{
        regobject::{numeric_regobject_oid_with_work, NumericRegobjectOid},
        resolution::bound_relation_identity_with_work,
        work::{inline, retire_entries, ReferenceWork, Work},
    },
    SQLError,
};
use uqa_core::{
    catalog_sequence::{SequenceOwner, SequenceOwnerDependency},
    memory::{
        Budgeted, BudgetedMap, MapKeyInput, MemoryBudget, OwnedMap, Produced, ProductionControl,
    },
    CancellationToken, RelationIdentity,
};

/// A captured named regclass argument uses the canonical relation namespace. Numeric inputs keep their deferred execution semantics even when no current relation has that OID; do not infer a catalog edge from a numeric value.
pub fn function_relation(
    reference: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<RelationIdentity>>, SQLError> {
    inline(function_relation_with_work(reference, control, &mut ()))
}

/// Read the same numeric input or canonical named relation under the invoking work window.
pub async fn function_relation_with_work(
    reference: &str,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Option<Produced<RelationIdentity>>, SQLError> {
    control.check()?;
    if matches!(
        numeric_regobject_oid_with_work(reference, control, work).await?,
        NumericRegobjectOid::Valid(_)
    ) {
        Ok(None)
    } else {
        bound_relation_identity_with_work(reference, control, work)
            .await
            .map(Some)
    }
}

/// Check stable owned-column edges and canonical implicit sequence references without inferring legacy owners, changing positions or allocating sequence values. Inherited SERIAL and identity columns may name another table's sequence; an internal owner must still point back to its original identity column. Legacy table counters remain valid without a sequence reference.
pub fn validate<'a>(
    tables: impl Iterator<Item = ([u8; 16], &'a [ColumnDef])>,
    sequences: impl Iterator<Item = (&'a RelationIdentity, Option<SequenceOwner>)>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    inline(validate_with_work(
        tables,
        sequences,
        memory,
        cancellation,
        &mut (),
    ))
}

/// Validate the same stable identities and canonical sequence names under a caller-supplied work window. Successful cleanup releases the original map nodes in bounded steps.
pub async fn validate_with_work<'a, W: Work + Send>(
    tables: impl Iterator<Item = ([u8; 16], &'a [ColumnDef])>,
    sequences: impl Iterator<Item = (&'a RelationIdentity, Option<SequenceOwner>)>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let mut input = ReferenceWork {
        control: &control,
        work,
    };
    input.control.check()?;
    let mut relations = BudgetedMap::new(memory);
    let mut columns = BudgetedMap::new(memory);
    for (table, definitions) in tables {
        input.checkpoint().await?;
        if relations.insert(table, definitions)?.is_some() {
            return Err(invalid("duplicate owner table identity"));
        }
        for column in definitions {
            input.checkpoint().await?;
            if let Some(id) = column.object_id {
                if columns.insert((table, id), column)?.is_some() {
                    return Err(invalid("duplicate owner column identity"));
                }
            }
        }
    }
    let mut names = OwnedMap::new();
    for (name, owner) in sequences {
        let key = Budgeted::new(name, memory.reserve(0)?);
        if names
            .insert_admitted_key_from(key, (), &mut input)
            .await?
            .is_some()
        {
            return Err(invalid("duplicate sequence name"));
        }
        if let Some(owner) = owner {
            let column = columns
                .get(&(owner.table_object_id, owner.column_object_id))
                .ok_or_else(|| invalid("missing owned column identity"))?;
            if owner.dependency == SequenceOwnerDependency::Internal {
                internal_owner(column, name, &mut input).await?;
            }
        }
    }
    for (_, definitions) in &relations {
        input.checkpoint().await?;
        for column in *definitions {
            input.checkpoint().await?;
            implicit_sequence(column, &names, &mut input).await?;
        }
    }
    input.control.check()?;
    retire_entries(names.into_iter(), &control, input.work).await?;
    retire_entries(columns.into_entries(), &control, input.work).await?;
    retire_entries(relations.into_entries(), &control, input.work).await
}

async fn internal_owner<W: Work + Send>(
    column: &ColumnDef,
    name: &RelationIdentity,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<(), SQLError> {
    let provenance = column
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
        .ok_or_else(|| invalid("internal owner is not an identity column"))?;
    let reference = provenance
        .sequence
        .as_deref()
        .ok_or_else(|| invalid("identity column has no sequence"))?;
    let original = bound_relation_identity_with_work(reference, input.control, input.work).await?;
    if input.compare(&*original, &name).await?.is_ne() {
        return Err(invalid("internal owner names another sequence"));
    }
    Ok(())
}

async fn implicit_sequence<W: Work + Send>(
    column: &ColumnDef,
    names: &OwnedMap<&RelationIdentity, ()>,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<(), SQLError> {
    let Some(provenance) = &column.auto_increment else {
        return Ok(());
    };
    if provenance.declaration.is_some() {
        return Err(invalid("unconsumed identity declaration"));
    }
    let Some(reference) = &provenance.sequence else {
        return if provenance.is_legacy() {
            Ok(())
        } else {
            Err(invalid("implicit column has no sequence"))
        };
    };
    let name = bound_relation_identity_with_work(reference, input.control, input.work).await?;
    if names.get_from(&*name, input).await?.is_none() {
        return Err(invalid("missing implicit sequence"));
    }
    Ok(())
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native sequence dependency: {reason}"))
}

#[cfg(test)]
mod tests;
