//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Secondary indexes, path indexes, and column statistics.

use super::{
    column_stats_key, column_stats_prefix, decode_catalog_relation_key, decode_value, encode_value,
    key_with_tag, load_single_string_rows, read_str, relation_key, single_str_key, string_value,
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, KeyValueBatch,
    KeyValueCatalog, RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult,
    StoredCatalogIndex, StoredColumnStats, TAG_CATALOG_INDEX, TAG_PATH_INDEX,
};

impl KeyValueCatalog {
    pub(super) fn save_catalog_index_impl(
        &self,
        relation: &RelationIdentity,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        let table =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        if relation.schema != table.schema {
            return Err(StorageBackendError::Other(format!(
                "catalog index `{}` cannot belong to a different schema than table `{}`",
                relation.qualified_name(),
                table.qualified_name()
            )));
        }
        if self
            .store
            .get(&relation_key(super::TAG_TABLE, &table)?)?
            .is_none()
        {
            return Err(StorageBackendError::Other(format!(
                "catalog index `{}` references missing table `{}`",
                relation.qualified_name(),
                table.qualified_name()
            )));
        }
        self.require_relation_kind(&table, RelationKind::Table)?;
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), relation, RelationKind::Index)?;
        batch.put(
            &relation_key(TAG_CATALOG_INDEX, relation)?,
            &encode_value(&StoredCatalogIndex {
                index_type: index_type.to_string(),
                table_name: table.qualified_name(),
                columns_json: columns_json.to_string(),
                parameters_json: parameters_json.to_string(),
            })?,
        )?;
        batch.commit()
    }

    pub(super) fn drop_catalog_index_impl(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_CATALOG_INDEX, relation)?)?;
        self.release_relation(batch.as_mut(), relation, RelationKind::Index)?;
        batch.commit()
    }

    pub(super) fn drop_catalog_indexes_for_table_impl(
        &self,
        table_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.drop_catalog_indexes_for_table_in_batch(batch.as_mut(), table_name)?;
        batch.commit()
    }

    pub(super) fn drop_catalog_indexes_for_table_in_batch(
        &self,
        batch: &mut dyn KeyValueBatch,
        table_name: &str,
    ) -> StorageBackendResult<()> {
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name {
                batch.delete(&relation_key(TAG_CATALOG_INDEX, &row.relation)?)?;
                self.release_relation(batch, &row.relation, RelationKind::Index)?;
            }
        }
        Ok(())
    }

    pub(super) fn load_catalog_indexes_impl(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
            let (relation, _, _) = decode_catalog_relation_key(&key)?;
            let stored: StoredCatalogIndex = decode_value(&value)?;
            rows.push(CatalogIndexRow {
                relation,
                index_type: stored.index_type,
                table_name: stored.table_name,
                columns_json: stored.columns_json,
                parameters_json: stored.parameters_json,
            });
        }
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }

    pub(super) fn save_path_index_impl(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_PATH_INDEX, graph_name)?,
            &string_value(label_sequences_json),
        )
    }

    pub(super) fn drop_path_index_impl(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_PATH_INDEX, graph_name)?)
    }

    pub(super) fn load_path_indexes_impl(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_PATH_INDEX)
    }

    pub(super) fn save_column_stats_impl(
        &self,
        stats: ColumnStatsInput<'_>,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &column_stats_key(stats.table_name, stats.column_name)?,
            &encode_value(&StoredColumnStats {
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_value.map(str::to_string),
                max_value: stats.max_value.map(str::to_string),
                row_count: stats.row_count,
                histogram_json: stats.histogram_json.to_string(),
                mcv_values_json: stats.mcv_values_json.to_string(),
                mcv_frequencies_json: stats.mcv_frequencies_json.to_string(),
            })?,
        )
    }

    pub(super) fn replace_column_stats_impl(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()> {
        let mut encoded = Vec::with_capacity(stats.len());
        for row in stats {
            if row.table_name != table_name {
                return Err(StorageBackendError::Other(format!(
                    "column stats row for table `{}` cannot be stored in snapshot `{table_name}`",
                    row.table_name
                )));
            }
            encoded.push((
                column_stats_key(row.table_name, row.column_name)?,
                encode_value(&StoredColumnStats {
                    distinct_count: row.distinct_count,
                    null_count: row.null_count,
                    min_value: row.min_value.map(str::to_string),
                    max_value: row.max_value.map(str::to_string),
                    row_count: row.row_count,
                    histogram_json: row.histogram_json.to_string(),
                    mcv_values_json: row.mcv_values_json.to_string(),
                    mcv_frequencies_json: row.mcv_frequencies_json.to_string(),
                })?,
            ));
        }
        let mut batch = self.store.batch();
        batch.delete_prefix(&column_stats_prefix(table_name)?)?;
        for (key, value) in encoded {
            batch.put(&key, &value)?;
        }
        batch.commit()
    }

    pub(super) fn load_column_stats_impl(
        &self,
        table_name: &str,
    ) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&column_stats_prefix(table_name)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let column_name = read_str(&key, &mut offset)?;
            let stored: StoredColumnStats = decode_value(&value)?;
            rows.push(ColumnStatsRow {
                column_name,
                distinct_count: stored.distinct_count,
                null_count: stored.null_count,
                min_value: stored.min_value,
                max_value: stored.max_value,
                row_count: stored.row_count,
                histogram_json: stored.histogram_json,
                mcv_values_json: stored.mcv_values_json,
                mcv_frequencies_json: stored.mcv_frequencies_json,
            });
        }
        rows.sort_by(|a, b| a.column_name.cmp(&b.column_name));
        Ok(rows)
    }

    pub(super) fn delete_column_stats_impl(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&column_stats_prefix(table_name)?)?;
        Ok(())
    }
}
