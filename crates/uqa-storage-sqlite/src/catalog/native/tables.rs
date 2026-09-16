//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table definitions and their object/generation bindings share the data session.

mod lifecycle;
mod owned;
mod rename;

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result,
    SQLiteError,
};
use crate::catalog::{RelationIdentity, RelationKind, TableSchema};
use rusqlite::types::ValueRef;

impl Catalog {
    pub(in crate::catalog) fn save_native_table(&self, schema: &TableSchema) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let name = schema.relation.qualified_name();
            snapshot.claim_relation(batch, &schema.relation, RelationKind::Table)?;
            let previous = snapshot.table_binding(&name)?;
            let (old_id, old_generation) = match previous {
                Some((
                    NativeRecordOwner::Object {
                        identity,
                        generation,
                    },
                    _,
                )) => (identity, generation),
                _ => ([0; 16], [0; 16]),
            };
            let identity = snapshot.allocate_identity(if schema.object_id == [0; 16] {
                old_id
            } else {
                schema.object_id
            })?;
            let generation =
                snapshot.allocate_identity(if schema.storage_generation == [0; 16] {
                    old_generation
                } else {
                    schema.storage_generation
                })?;
            let owner = NativeRecordOwner::Object {
                identity,
                generation,
            };
            snapshot.check_table_identity(&name, identity)?;
            if let Some((old, _)) = previous {
                if old != owner {
                    snapshot.transfer_table_data(batch, old, owner, &name)?;
                    snapshot.delete_prefix(batch, Family::Tables, old, &[])?;
                    snapshot.rename_table_catalog_indexes(
                        batch,
                        &schema.relation,
                        &schema.relation,
                    )?;
                }
            }
            if previous != Some((owner, true)) {
                snapshot.put_table_binding(batch, &name, owner, true)?;
            }
            let fields = serde_json::to_string(&schema.fts_fields)?;
            let vectors = serde_json::to_string(&schema.vector_fields)?;
            let acl = schema.acl.as_ref().map(serde_json::to_string).transpose()?;
            let columns = serde_json::to_string(&schema.column_acls)?;
            snapshot.put_row(
                batch,
                Family::Tables,
                owner,
                &[
                    text(&schema.relation.schema),
                    text(&schema.relation.name),
                    text("table"),
                    text(&schema.analyzer_json),
                    text(&fields),
                    text(&vectors),
                    text(&schema.columns_json),
                    text(&schema.constraints_json),
                    ValueRef::Blob(&generation),
                    ValueRef::Blob(&identity),
                    text(&schema.role_owner),
                    optional_text(acl.as_deref()),
                    text(&columns),
                ],
            )
        })
    }

    pub(in crate::catalog) fn load_native_tables(&self) -> Result<Option<Vec<TableSchema>>> {
        self.read_native(|snapshot| {
            let mut tables = Vec::new();
            snapshot.visit_rows(Family::Tables, None, &[], |row| {
                let identity = |value: ValueRef<'_>| -> Result<[u8; 16]> {
                    value
                        .as_blob()
                        .ok()
                        .and_then(|value| value.try_into().ok())
                        .ok_or_else(|| {
                            SQLiteError::StorageBackend("invalid native table identity".into())
                        })
                };
                tables.push(TableSchema {
                    relation: RelationIdentity::new(string(row[0])?, string(row[1])?),
                    analyzer_json: string(row[3])?,
                    fts_fields: serde_json::from_str(&string(row[4])?)?,
                    vector_fields: serde_json::from_str(&string(row[5])?)?,
                    columns_json: if row[6] == ValueRef::Null {
                        String::new()
                    } else {
                        string(row[6])?
                    },
                    constraints_json: string(row[7])?,
                    storage_generation: identity(row[8])?,
                    object_id: identity(row[9])?,
                    role_owner: string(row[10])?,
                    acl: if row[11] == ValueRef::Null {
                        None
                    } else {
                        Some(serde_json::from_str(&string(row[11])?)?)
                    },
                    column_acls: if row[12] == ValueRef::Null {
                        std::collections::BTreeMap::new()
                    } else {
                        serde_json::from_str(&string(row[12])?)?
                    },
                });
                Ok(())
            })?;
            tables.sort_unstable_by(|left, right| left.relation.cmp(&right.relation));
            Ok(tables)
        })
    }
}
