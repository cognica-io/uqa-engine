//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema and column-statistics restoration.

use super::{
    BTreeMap, CatalogFacade, ColumnStatsRow, Engine, StorageBackendError, StorageBackendResult,
    Value,
};

impl Engine {
    pub(super) fn restore_schemas_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let schemas = catalog.load_schemas()?;
        for schema in &schemas {
            Self::validate_schema_name(schema)?;
        }
        if !schemas.iter().any(|name| name == "public") {
            return Err(StorageBackendError::Other(
                "catalog is missing required schema `public`".to_string(),
            ));
        }
        *self.schemas.write() = schemas.into_iter().collect();
        Ok(())
    }

    pub(crate) fn load_column_stats_from_catalog(
        catalog: &dyn CatalogFacade,
        table_name: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        let mut out = BTreeMap::new();
        for row in catalog.load_column_stats(table_name)? {
            out.insert(row.column_name.clone(), Self::column_stats_from_row(row)?);
        }
        Ok(out)
    }

    fn column_stats_from_row(
        row: ColumnStatsRow,
    ) -> StorageBackendResult<uqa_planner::ColumnStats> {
        Ok(uqa_planner::ColumnStats {
            distinct_count: row.distinct_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative distinct_count for column `{}`",
                    row.column_name
                ))
            })?,
            null_count: row.null_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative null_count for column `{}`",
                    row.column_name
                ))
            })?,
            min_value: Self::decode_column_stat_value(row.min_value)?,
            max_value: Self::decode_column_stat_value(row.max_value)?,
            row_count: row.row_count.try_into().map_err(|_| {
                StorageBackendError::Other(format!(
                    "negative row_count for column `{}`",
                    row.column_name
                ))
            })?,
            histogram: serde_json::from_str(&row.histogram_json)?,
            mcv_values: serde_json::from_str(&row.mcv_values_json)?,
            mcv_frequencies: serde_json::from_str(&row.mcv_frequencies_json)?,
        })
    }

    fn decode_column_stat_value(raw: Option<String>) -> StorageBackendResult<Option<Value>> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        match serde_json::from_str::<Value>(&raw)? {
            Value::Null => Ok(None),
            value => Ok(Some(value)),
        }
    }
}
