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
