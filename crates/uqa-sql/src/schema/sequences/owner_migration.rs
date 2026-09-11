//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Infer stable sequence owners from legacy table and foreign-column declarations.
use super::implicit::stored_owner_names_current;
use uqa_core::{
    catalog_sequence::{SequenceOwner, SequenceOwnerDependency},
    RelationIdentity,
};
pub fn resolve_migrated_sequence_reference(
    reference: &str,
    sequences: &[RelationIdentity],
) -> Result<RelationIdentity, String> {
    let (schema, name) = RelationIdentity::parse_reference(reference)?;
    let candidates = sequences
        .iter()
        .filter(|candidate| {
            candidate.name == name
                && schema
                    .as_ref()
                    .is_none_or(|schema| candidate.schema == *schema)
        })
        .cloned()
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err(format!(
            "implicit sequence owner references missing sequence `{reference}`"
        )),
        _ => Err(format!(
            "implicit sequence owner reference `{reference}` is ambiguous"
        )),
    }
}
pub fn collect_migrated_sequence_owner(
    relation: &RelationIdentity,
    table_object_id: [u8; 16],
    column: &crate::ast::ColumnDef,
    sequence_relations: &[RelationIdentity],
    valid_owners: &mut std::collections::BTreeSet<([u8; 16], [u8; 16])>,
    inferred: &mut std::collections::BTreeMap<RelationIdentity, SequenceOwner>,
) -> Result<(), String> {
    let relation_name = relation.qualified_name();
    let column_object_id = column.object_id.ok_or_else(|| {
        format!(
            "column `{relation_name}`.`{}` has no object identity during sequence-owner migration",
            column.name
        )
    })?;
    valid_owners.insert((table_object_id, column_object_id));
    let Some(provenance) = column.auto_increment.as_ref() else {
        return Ok(());
    };
    let Some(named_owner) = provenance.owner.as_ref() else {
        return Ok(());
    };
    if !stored_owner_names_current(relation, column, named_owner) {
        return Ok(());
    }
    let Some(sequence) = provenance.sequence.as_deref() else {
        return Ok(());
    };
    let sequence = resolve_migrated_sequence_reference(sequence, sequence_relations)?;
    let owner = SequenceOwner {
        table_object_id,
        column_object_id,
        dependency: if provenance.is_identity() {
            SequenceOwnerDependency::Internal
        } else {
            SequenceOwnerDependency::Automatic
        },
    };
    if inferred
        .insert(sequence.clone(), owner)
        .is_some_and(|old| old != owner)
    {
        return Err(format!(
            "sequence `{}` has conflicting implicit owners",
            sequence.qualified_name()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
