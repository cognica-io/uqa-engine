//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog index definitions retain their relation claims and backing-table references atomically.

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result,
    SQLiteError,
};
use crate::catalog::{CatalogIndexRow, RelationIdentity, RelationKind};
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueBatch;

impl NativeSnapshot {
    pub(in crate::catalog::native) fn rename_table_catalog_indexes(
        &self,
        batch: &mut dyn KeyValueBatch,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<()> {
        let owner = NativeRecordOwner::Database(self.database);
        self.visit_rows(Family::CatalogIndexes, Some(owner), &[], |row| {
            if row[4] == text(&from.schema) && row[5] == text(&from.name) {
                self.put_row(
                    batch,
                    Family::CatalogIndexes,
                    owner,
                    &[
                        row[0],
                        row[1],
                        row[2],
                        row[3],
                        text(&to.schema),
                        text(&to.name),
                        row[6],
                        row[7],
                        row[8],
                    ],
                )?;
            }
            Ok(())
        })
    }

    pub(in crate::catalog::native) fn drop_table_catalog_indexes(
        &self,
        batch: &mut dyn KeyValueBatch,
        table: &RelationIdentity,
    ) -> Result<()> {
        let owner = NativeRecordOwner::Database(self.database);
        self.visit_rows(Family::CatalogIndexes, Some(owner), &[], |row| {
            if row[4] == text(&table.schema) && row[5] == text(&table.name) {
                // The row's validated kind and composite foreign key establish this claim without a nested persistence read.
                self.delete_prefix(batch, Family::CatalogIndexes, owner, &[row[0], row[1]])?;
                self.delete_prefix(batch, Family::Relations, owner, &[row[0], row[1]])?;
            }
            Ok(())
        })
    }
}

impl Catalog {
    pub(in crate::catalog) fn save_native_catalog_index(
        &self,
        index: &CatalogIndexRow,
        table: &RelationIdentity,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let backing = match snapshot.table_binding(&table.qualified_name())? {
                Some((owner, true)) => snapshot.contains_row(Family::Tables, owner, &[])?,
                _ => false,
            };
            if !backing || snapshot.relation_kind(table)?.as_deref() != Some("table") {
                return Err(SQLiteError::StorageBackend(format!(
                    "table `{}` does not exist",
                    table.qualified_name()
                )));
            }
            snapshot.claim_relation(batch, &index.relation, RelationKind::Index)?;
            snapshot.put_row(
                batch,
                Family::CatalogIndexes,
                NativeRecordOwner::Database(snapshot.database),
                &[
                    text(&index.relation.schema),
                    text(&index.relation.name),
                    text("index"),
                    text(&index.index_type),
                    text(&table.schema),
                    text(&table.name),
                    text(&index.columns_json),
                    text(&index.parameters_json),
                    optional_text(index.definition_json.as_deref()),
                ],
            )
        })
    }

    pub(in crate::catalog) fn drop_native_catalog_index(
        &self,
        relation: &RelationIdentity,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.release_relation(batch, relation, RelationKind::Index)?;
            snapshot.delete_prefix(
                batch,
                Family::CatalogIndexes,
                NativeRecordOwner::Database(snapshot.database),
                &[text(&relation.schema), text(&relation.name)],
            )
        })
    }

    pub(in crate::catalog) fn drop_native_table_indexes(
        &self,
        table: &RelationIdentity,
    ) -> Result<Option<()>> {
        self.conn
            .with_native_write(|snapshot, batch| snapshot.drop_table_catalog_indexes(batch, table))
    }

    pub(in crate::catalog) fn load_native_catalog_indexes(
        &self,
    ) -> Result<Option<Vec<CatalogIndexRow>>> {
        self.read_native(|snapshot| {
            let mut indexes = Vec::new();
            snapshot.visit_rows(
                Family::CatalogIndexes,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    indexes.push(CatalogIndexRow {
                        relation: RelationIdentity::new(string(row[0])?, string(row[1])?),
                        index_type: string(row[3])?,
                        table_name: RelationIdentity::new(string(row[4])?, string(row[5])?)
                            .qualified_name(),
                        columns_json: string(row[6])?,
                        parameters_json: string(row[7])?,
                        definition_json: if row[8] == ValueRef::Null {
                            None
                        } else {
                            Some(string(row[8])?)
                        },
                    });
                    Ok(())
                },
            )?;
            Ok(indexes)
        })
    }
}
