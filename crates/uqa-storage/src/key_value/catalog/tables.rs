//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table definitions and exact-name data lifecycles are evaluated and published together.

use super::super::{table_owners, KeyValueRead};
use super::{
    decode_relation_key, decode_value, encode_value, relation_key, single_str_key, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult, StoredCatalogIndex,
    TableSchema, TAG_CATALOG_INDEX, TAG_METADATA, TAG_RELATION, TAG_SCHEMA, TAG_TABLE,
};
use super::{indexes, relations, table_data};
use crate::document_store::identifiers::legacy_document_id_metadata_key;

fn table(
    read: &dyn KeyValueRead,
    relation: &RelationIdentity,
) -> StorageBackendResult<Option<TableSchema>> {
    read.get(&relation_key(TAG_TABLE, relation)?)?
        .map(|value| {
            let schema: TableSchema = decode_value(&value)?;
            if &schema.relation != relation {
                return Err(StorageBackendError::Other(
                    "table definition disagrees with its catalog key".into(),
                ));
            }
            Ok(schema)
        })
        .transpose()
}

impl KeyValueCatalog {
    pub(super) fn save_table_impl(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        let durable = self.store.identifier_allocator().is_some();
        self.store.with_mutation(&mut |read, batch| {
            relations::claim_relation(read, batch, &schema.relation, RelationKind::Table)?;
            let schema = if durable {
                batch.require_unchanged(&single_str_key(TAG_SCHEMA, &schema.relation.schema)?)?;
                table_owners::prepare_table(read, batch, schema)?
            } else {
                schema.clone()
            };
            super::relation_acl::clear(read, batch, &schema.relation)?;
            batch.put(
                &relation_key(TAG_TABLE, &schema.relation)?,
                &encode_value(&schema)?,
            )
        })
    }

    pub(super) fn load_tables_impl(&self) -> StorageBackendResult<Vec<TableSchema>> {
        let durable = self.store.identifier_allocator().is_some();
        let mut rows = Vec::new();
        self.store.with_read_view(&mut |read| {
            read.visit_prefix(&[TAG_TABLE], &mut |key, value| {
                let relation = decode_relation_key(key)?;
                let schema: TableSchema = decode_value(value)?;
                if schema.relation != relation {
                    return Err(StorageBackendError::Other(format!(
                        "table catalog key `{}` disagrees with stored relation `{}`",
                        relation.qualified_name(),
                        schema.relation.qualified_name()
                    )));
                }
                rows.push(schema);
                Ok(())
            })?;
            if durable {
                let mut objects = std::collections::BTreeSet::new();
                for schema in &rows {
                    table_owners::validate_table(read, schema)?;
                    if schema.object_id != [0; 16] && !objects.insert(schema.object_id) {
                        return Err(StorageBackendError::Other(
                            "duplicate table object identity".into(),
                        ));
                    }
                }
            }
            let acls = super::relation_acl::load(read)?;
            for schema in &mut rows {
                acls.apply(&schema.relation, &mut schema.security)?;
            }
            Ok(())
        })?;
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }

    fn drop_table_owned(&self, name: &str, data: bool) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let durable = self.store.identifier_allocator().is_some();
        self.store.with_mutation(&mut |read, batch| {
            super::relation_acl::clear(read, batch, &relation)?;
            indexes::drop_table_indexes(read, batch, &relation.qualified_name())?;
            for name in relation.canonical_and_legacy_public_names() {
                if durable {
                    table_owners::drop_binding(read, batch, &name, data)?;
                }
                if data {
                    table_data::clear(batch, &name, true)?;
                    batch.delete(&single_str_key(
                        TAG_METADATA,
                        &legacy_document_id_metadata_key(&name),
                    )?)?;
                } else {
                    batch.reset_occurrences(&name)?;
                }
            }
            batch.delete(&relation_key(TAG_TABLE, &relation)?)?;
            relations::release_relation(read, batch, &relation, RelationKind::Table)
        })
    }

    pub(super) fn drop_table_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_table_owned(name, false)
    }
    pub(super) fn drop_table_and_data_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_table_owned(name, true)
    }

    pub(super) fn purge_table_data_impl(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let durable = self.store.identifier_allocator().is_some();
        self.store.with_mutation(&mut |read, batch| {
            for name in relation.canonical_and_legacy_public_names() {
                if durable {
                    table_owners::preserve_and_fence(read, batch, &name)?;
                }
                table_data::clear(batch, &name, false)?;
            }
            Ok(())
        })
    }

    pub(super) fn rename_table_data_impl(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let from_relation =
            RelationIdentity::from_legacy_name(from).map_err(StorageBackendError::Other)?;
        let to_relation =
            RelationIdentity::from_legacy_name(to).map_err(StorageBackendError::Other)?;
        if from_relation == to_relation {
            return Ok(());
        }
        if from_relation.schema != to_relation.schema {
            return Err(StorageBackendError::Other(
                "moving a table between schemas is not supported by the catalog".into(),
            ));
        }
        let durable = self.store.identifier_allocator().is_some();
        self.store.with_mutation(&mut |read, batch| {
            relations::require_schema_exists(read, &to_relation)?;
            if read.get(&relation_key(TAG_TABLE, &to_relation)?)?.is_some()
                || read
                    .get(&relation_key(TAG_RELATION, &to_relation)?)?
                    .is_some()
            {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists",
                    to_relation.qualified_name()
                )));
            }
            let mut schema = table(read, &from_relation)?.ok_or_else(|| {
                StorageBackendError::Other(format!("table `{from}` does not exist"))
            })?;
            relations::require_relation_kind(read, &from_relation, RelationKind::Table)?;
            if durable {
                batch.require_unchanged(&single_str_key(TAG_SCHEMA, &to_relation.schema)?)?;
                schema = table_owners::rename_bindings(read, batch, from, to, &schema, |name| {
                    table_data::has_data(read, name)
                })?;
            } else {
                schema.relation = to_relation.clone();
            }
            table_data::rename(read, batch, from, to)?;
            super::relation_acl::rename(read, batch, &from_relation, &to_relation)?;
            batch.put(
                &relation_key(TAG_TABLE, &to_relation)?,
                &encode_value(&schema)?,
            )?;
            batch.delete(&relation_key(TAG_TABLE, &from_relation)?)?;
            relations::release_relation(read, batch, &from_relation, RelationKind::Table)?;
            relations::claim_relation(read, batch, &to_relation, RelationKind::Table)?;
            for row in indexes::load_indexes(read)? {
                if row.table_name == from_relation.qualified_name() {
                    batch.put(
                        &relation_key(TAG_CATALOG_INDEX, &row.relation)?,
                        &encode_value(&StoredCatalogIndex {
                            index_type: row.index_type,
                            table_name: to_relation.qualified_name(),
                            columns_json: row.columns_json,
                            parameters_json: row.parameters_json,
                            definition_json: row.definition_json,
                        })?,
                    )?;
                }
            }
            Ok(())
        })
    }
}
