//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sequence dependencies retain original table/column identities and bound names.

use crate::{
    ast::ColumnDef,
    catalog::{
        regobject::{numeric_regobject_oid, NumericRegobjectOid},
        resolution::bound_relation_identity_with_control,
    },
    SQLError,
};
use uqa_core::{
    catalog_sequence::{SequenceOwner, SequenceOwnerDependency},
    memory::{BudgetedMap, MemoryBudget, Produced, ProductionControl},
    CancellationToken, RelationIdentity,
};

/// A captured named regclass argument uses the canonical relation namespace. Numeric inputs keep their deferred execution semantics even when no current relation has that OID; do not infer a catalog edge from a numeric value.
pub fn function_relation(
    reference: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<RelationIdentity>>, SQLError> {
    control.check()?;
    if matches!(
        numeric_regobject_oid(reference),
        NumericRegobjectOid::Valid(_)
    ) {
        Ok(None)
    } else {
        bound_relation_identity_with_control(reference, control).map(Some)
    }
}

/// Check stable owned-column edges and canonical implicit sequence references without inferring legacy owners, changing positions or allocating sequence values. Inherited SERIAL and identity columns may name another table's sequence; an internal owner must still point back to its original identity column. Legacy table counters remain valid without a sequence reference.
pub fn validate<'a>(
    tables: impl Iterator<Item = ([u8; 16], &'a [ColumnDef])>,
    sequences: impl Iterator<Item = (&'a RelationIdentity, Option<SequenceOwner>)>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut relations = BudgetedMap::new(memory);
    let mut columns = BudgetedMap::new(memory);
    for (table, definitions) in tables {
        control.check()?;
        if relations.insert(table, definitions)?.is_some() {
            return Err(invalid("duplicate owner table identity"));
        }
        for column in definitions {
            control.check()?;
            if let Some(id) = column.object_id {
                if columns.insert((table, id), column)?.is_some() {
                    return Err(invalid("duplicate owner column identity"));
                }
            }
        }
    }
    let mut names = BudgetedMap::new(memory);
    for (name, owner) in sequences {
        control.check()?;
        if names.insert(name, ())?.is_some() {
            return Err(invalid("duplicate sequence name"));
        }
        let Some(owner) = owner else {
            continue;
        };
        let column = columns
            .get(&(owner.table_object_id, owner.column_object_id))
            .ok_or_else(|| invalid("missing owned column identity"))?;
        if owner.dependency == SequenceOwnerDependency::Internal {
            let provenance = column
                .auto_increment
                .as_ref()
                .filter(|provenance| provenance.is_identity())
                .ok_or_else(|| invalid("internal owner is not an identity column"))?;
            let reference = provenance
                .sequence
                .as_deref()
                .ok_or_else(|| invalid("identity column has no sequence"))?;
            if &*bound_relation_identity_with_control(reference, &control)? != name {
                return Err(invalid("internal owner names another sequence"));
            }
        }
    }
    for (_, definitions) in &relations {
        for column in *definitions {
            control.check()?;
            let Some(provenance) = &column.auto_increment else {
                continue;
            };
            if provenance.declaration.is_some() {
                return Err(invalid("unconsumed identity declaration"));
            }
            let Some(reference) = &provenance.sequence else {
                if provenance.is_legacy() {
                    continue;
                }
                return Err(invalid("implicit column has no sequence"));
            };
            let name = bound_relation_identity_with_control(reference, &control)?;
            if !names.contains_key(&*name) {
                return Err(invalid("missing implicit sequence"));
            }
        }
    }
    control.check()?;
    Ok(())
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native sequence dependency: {reason}"))
}

#[cfg(test)]
mod tests;
