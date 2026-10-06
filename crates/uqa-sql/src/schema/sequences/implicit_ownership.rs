//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve declared SERIAL and IDENTITY owners against the loaded sequence namespace.
use super::implicit::stored_owner_names_current;
use crate::ast::ColumnDef;
use uqa_core::RelationIdentity;

pub trait StoredSequenceNames {
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String>;
}
pub struct BoundImplicitSequenceOwner {
    pub sequence: String,
    pub table_object_id: [u8; 16],
    pub column_object_id: [u8; 16],
    pub identity: bool,
}
pub fn bind_implicit_sequence_owners(
    catalog: &dyn StoredSequenceNames,
    table_name: &str,
    table_object_id: [u8; 16],
    columns: &[ColumnDef],
) -> Result<Vec<BoundImplicitSequenceOwner>, String> {
    let relation = RelationIdentity::from_legacy_name(table_name)?;
    let mut bindings = Vec::new();
    for column in columns {
        let Some(provenance) = column.auto_increment.as_ref() else {
            continue;
        };
        let Some(named_owner) = provenance.owner.as_ref() else {
            continue;
        };
        if !stored_owner_names_current(&relation, column, named_owner) {
            continue;
        }
        let Some(sequence) = provenance.sequence.as_deref() else {
            continue;
        };
        let sequence = catalog.stored_sequence_name(sequence)?;
        let column_object_id = column.object_id.ok_or_else(|| {
            format!(
                "column `{table_name}`.`{}` has no object identity",
                column.name
            )
        })?;
        bindings.push(BoundImplicitSequenceOwner {
            sequence,
            table_object_id,
            column_object_id,
            identity: provenance.is_identity(),
        });
    }
    Ok(bindings)
}

/// Remove legacy named owner markers for one canonically identified sequence.
pub fn clear_auto_increment_owner_markers(
    columns: &mut [crate::ast::ColumnDef],
    target: &uqa_core::RelationIdentity,
) -> bool {
    let mut changed = false;
    for column in columns {
        let Some(provenance) = column.auto_increment.as_mut() else {
            continue;
        };
        if provenance.owner.is_some()
            && provenance.sequence.as_deref().is_some_and(|reference| {
                crate::schema::dependencies::rewrites::stored_relation_reference_matches(
                    reference, target,
                )
            })
        {
            provenance.owner = None;
            changed = true;
        }
    }
    changed
}

/// Resolve a declaration's owners after its columns and constraints exist, including the table name in an explicitly named sequence's schema.
pub fn bind_declared_sequence_owners(
    names: &dyn StoredSequenceNames,
    catalog: &dyn super::ownership::SequenceOwnerCatalog,
    table_name: &str,
    table_object_id: [u8; 16],
    columns: &[ColumnDef],
) -> Result<Vec<BoundImplicitSequenceOwner>, crate::SQLError> {
    let mut bindings = bind_implicit_sequence_owners(names, table_name, table_object_id, columns)
        .map_err(crate::SQLError::Internal)?;
    let relation =
        RelationIdentity::from_legacy_name(table_name).map_err(crate::SQLError::Internal)?;
    for column in columns {
        let Some(provenance) = column.auto_increment.as_ref() else {
            continue;
        };
        let (Some(owner), Some(sequence)) = (&provenance.owner, &provenance.sequence) else {
            continue;
        };
        if stored_owner_names_current(&relation, column, owner) {
            continue;
        }
        let sequence = names
            .stored_sequence_name(sequence)
            .map_err(crate::SQLError::Internal)?;
        let ownership = crate::ast::SequenceOwnership::Column {
            table: owner.table.clone(),
            column: owner.column.clone(),
        };
        let Some(identity) = super::ownership::bind_sequence_owner(catalog, &sequence, &ownership)?
        else {
            return Err(crate::SQLError::Internal(
                "declared sequence owner disappeared".into(),
            ));
        };
        bindings.push(BoundImplicitSequenceOwner {
            sequence,
            table_object_id: identity.table_object_id,
            column_object_id: identity.column_object_id,
            identity: provenance.is_identity(),
        });
    }
    Ok(bindings)
}

/// Resolve the identity sequence owned by this column or its declarative-partition ancestor. A sequence explicitly linked to an unrelated table is not its DEFAULT source.
pub fn identity_column_sequence(
    catalog: &dyn crate::semantics::partition::PartitionCatalog,
    table: &str,
    column: &ColumnDef,
) -> Result<Option<String>, crate::SQLError> {
    use crate::SQLError;
    let Some(provenance) = column
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
    else {
        return Ok(None);
    };
    let missing = || SQLError::Routine {
        sqlstate: "XX000".into(),
        message: "no owned sequence found".into(),
    };
    let owner = provenance.owner.as_ref().ok_or_else(missing)?;
    let mut current = table.to_string();
    let mut visited = std::collections::BTreeSet::new();
    loop {
        let relation = RelationIdentity::from_legacy_name(&current).map_err(SQLError::Internal)?;
        if stored_owner_names_current(&relation, column, owner) {
            break;
        }
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(
                "cycle in identity sequence ancestry".into(),
            ));
        }
        let hierarchy = catalog
            .try_table_hierarchy(&current)
            .map_err(SQLError::Internal)?;
        if !hierarchy.is_partition() {
            return Err(missing());
        }
        current.clone_from(hierarchy.parents.first().ok_or_else(missing)?);
    }
    provenance.sequence.clone().map(Some).ok_or_else(missing)
}

#[cfg(test)]
mod tests;
