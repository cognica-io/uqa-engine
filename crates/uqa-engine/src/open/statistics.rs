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
use crate::statistics::value_size;

impl Engine {
    pub(super) fn restore_schemas_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let schemas = catalog.load_schema_rows()?;
        for schema in &schemas {
            Self::validate_schema_name(&schema.name)?;
        }
        *self.durable.schemas.write() = schemas
            .into_iter()
            .map(crate::state::SchemaSecurity::from_row)
            .collect();
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
        // Old catalogs may contain entire documents in statistical values.
        // Bound decoding too, so opening one does not reconstruct those large
        // strings before the automatic worker replaces the legacy snapshot.
        let histogram =
            Self::decode_column_stat_list(&row.histogram_json, value_size::HISTOGRAM_VALUES)?;
        let (mcv_values, mcv_frequencies) = if row.mcv_values_json.len()
            > value_size::encoded_list_bytes(value_size::MCV_VALUES)
            || row.mcv_frequencies_json.len() > value_size::MCV_VALUES * 64
        {
            (Vec::new(), Vec::new())
        } else {
            let values: Vec<Value> = serde_json::from_str(&row.mcv_values_json)?;
            let frequencies: Vec<f64> = serde_json::from_str(&row.mcv_frequencies_json)?;
            if values.len() > value_size::MCV_VALUES {
                (Vec::new(), Vec::new())
            } else if values.len() != frequencies.len() {
                return Err(StorageBackendError::Other(format!(
                    "mismatched MCV values and frequencies for column `{}`",
                    row.column_name
                )));
            } else {
                values
                    .into_iter()
                    .zip(frequencies)
                    .filter(|(value, _)| value_size::accepts(value))
                    .unzip()
            }
        };
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
            histogram,
            mcv_values,
            mcv_frequencies,
        })
    }

    fn decode_column_stat_list(raw: &str, limit: usize) -> StorageBackendResult<Vec<Value>> {
        if raw.len() > value_size::encoded_list_bytes(limit) {
            return Ok(Vec::new());
        }
        let values: Vec<Value> = serde_json::from_str(raw)?;
        if values.len() > limit {
            return Ok(Vec::new());
        }
        Ok(values.into_iter().filter(value_size::accepts).collect())
    }

    fn decode_column_stat_value(raw: Option<String>) -> StorageBackendResult<Option<Value>> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        if raw.len() > value_size::ENCODED_VALUE_BYTES {
            return Ok(None);
        }
        match serde_json::from_str::<Value>(&raw)? {
            Value::Null => Ok(None),
            value => Ok(value_size::accepts(&value).then_some(value)),
        }
    }
}
