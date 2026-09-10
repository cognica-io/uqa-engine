//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence ownership to stable table and column identities.
use crate::ast::{ColumnDef, SequenceOwnership};
use crate::SQLError;
use uqa_core::RelationIdentity;

pub type SequenceOwnerColumns = ([u8; 16], Vec<ColumnDef>);

pub trait SequenceOwnerCatalog {
    fn resolve_owner_relation(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError>;
    fn owner_table_columns(
        &self,
        canonical: &str,
    ) -> Result<Option<SequenceOwnerColumns>, SQLError>;
    fn owner_foreign_columns(&self, relation: &RelationIdentity) -> Option<SequenceOwnerColumns>;
}

#[derive(Debug, Clone, Copy)]
pub struct SequenceOwnerColumnIdentity {
    pub table_object_id: [u8; 16],
    pub column_object_id: [u8; 16],
}

pub fn sequence_owner_column_identity(
    catalog: &dyn SequenceOwnerCatalog,
    canonical: &str,
    relation_kind: &str,
    column_name: &str,
) -> Result<Option<SequenceOwnerColumnIdentity>, SQLError> {
    let relation = RelationIdentity::from_legacy_name(canonical).map_err(|error| {
        SQLError::Internal(format!("resolve sequence owner `{canonical}`: {error}"))
    })?;
    let columns = match relation_kind {
        "table" => catalog
            .owner_table_columns(canonical)?
            .ok_or_else(|| SQLError::Internal(format!("table `{canonical}` disappeared")))?,
        "foreign table" => catalog.owner_foreign_columns(&relation).ok_or_else(|| {
            SQLError::Internal(format!("foreign table `{canonical}` disappeared"))
        })?,
        _ => return Ok(None),
    };
    let column_object_id = columns
        .1
        .iter()
        .find(|column| column.name == column_name)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42703".into(),
            message: format!(
                "column \"{column_name}\" of relation \"{}\" does not exist",
                relation.name
            ),
        })?
        .object_id
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "column `{canonical}`.`{column_name}` has no object identity"
            ))
        })?;
    Ok(Some(SequenceOwnerColumnIdentity {
        table_object_id: columns.0,
        column_object_id,
    }))
}

pub fn bind_sequence_owner(
    catalog: &dyn SequenceOwnerCatalog,
    sequence_name: &str,
    ownership: &SequenceOwnership,
) -> Result<Option<SequenceOwnerColumnIdentity>, SQLError> {
    let SequenceOwnership::Column { table, column } = ownership else {
        return Ok(None);
    };
    let (table_name, kind) =
        catalog
            .resolve_owner_relation(table)?
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{table}\" does not exist"),
            })?;
    let Some(owner_column) = sequence_owner_column_identity(catalog, &table_name, kind, column)?
    else {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("sequence cannot be owned by relation \"{table_name}\""),
        });
    };
    let sequence_relation = RelationIdentity::from_legacy_name(sequence_name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve sequence `{sequence_name}` ownership: {error}"
        ))
    })?;
    let table_relation = RelationIdentity::from_legacy_name(&table_name).map_err(|error| {
        SQLError::Internal(format!("resolve table `{table_name}` ownership: {error}"))
    })?;
    if sequence_relation.schema != table_relation.schema {
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: "sequence must be in same schema as table it is linked to".into(),
        });
    }
    Ok(Some(owner_column))
}

pub fn require_sequence_ownership(
    local_name: &str,
    has_owner_privileges: bool,
) -> Result<(), SQLError> {
    if has_owner_privileges {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("must be owner of sequence {local_name}"),
    })
}
pub fn reject_owned_sequence_role_change(
    local_name: &str,
    has_column_owner: bool,
) -> Result<(), SQLError> {
    if !has_column_owner {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!("cannot change owner of sequence \"{local_name}\""),
    })
}
