//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-statistics collection, persistence, and lazy refresh.

use super::{
    build_histogram, build_mcv, collect_analyze_values, distinct_count, Arc, BTreeMap,
    CatalogFacade, ColumnStatsInput, DocId, Engine, Ordering, RelationIdentity,
    StorageBackendError, StorageBackendResult, TableState, Value,
};

struct HierarchyAnalyzeInputs {
    row_count: u64,
    values: BTreeMap<String, Vec<Value>>,
    null_counts: BTreeMap<String, u64>,
}

impl Engine {
    /// Refresh per-column statistics for one table, or every table when
    /// `table` is `None`. The analysis scans each document and collects per-
    /// column distinct count / null count / min / max / equi-depth
    /// histogram (100 buckets) / MCV list (top 10 above-average
    /// frequency), and stores the result on the per-table state so the
    /// cardinality estimator can read it on subsequent queries.
    pub fn run_analyze(&self, table: Option<&str>) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| engine.run_analyze_inner(table))
    }

    fn run_analyze_inner(&self, table: Option<&str>) -> StorageBackendResult<()> {
        if let Some(name) = table {
            let Some(canonical_name) = self.try_resolve_table_name(name)? else {
                return Err(StorageBackendError::Other(format!(
                    "ANALYZE target table `{name}` does not exist"
                )));
            };
            let Some(table) = self.try_table(&canonical_name)? else {
                return Err(StorageBackendError::Other(format!(
                    "ANALYZE target table `{name}` does not exist"
                )));
            };
            self.analyze_table(&canonical_name, &table, true)?;
        } else {
            // The catalog can change between collecting the names and opening
            // each table in another session. Missing entries are only benign
            // for the catalog-wide form; an explicitly named table above is
            // always an error.
            let names: Vec<String> = self
                .storage
                .tables
                .read()
                .keys()
                .map(RelationIdentity::qualified_name)
                .collect();
            for name in names {
                let Some(table) = self.table(&name)? else {
                    continue;
                };
                self.analyze_table(&name, &table, true)?;
            }
        }
        // Persisted statistics participate in DPccp join ordering and every
        // cached optimized statement. Publish the same commit-delayed data
        // generation even when ANALYZE did not change document contents.
        self.note_table_data_changed();
        Ok(())
    }

    pub(crate) fn mark_column_stats_dirty(
        &self,
        canonical_table_name: &str,
        table: &Arc<TableState>,
    ) -> StorageBackendResult<()> {
        let ancestors = self
            .hierarchy_ancestor_tables(canonical_table_name)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        for name in ancestors {
            let state = if name == canonical_table_name {
                Arc::clone(table)
            } else {
                self.try_table(&name)?.ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "statistics ancestor table `{name}` does not exist"
                    ))
                })?
            };
            if state.persistence != uqa_sql::ast::RelationPersistence::Temporary
                && !state.column_stats_dirty.load(Ordering::Acquire)
            {
                if let Some(catalog) = self.storage.catalog.as_ref() {
                    catalog.delete_column_stats(&name)?;
                }
            }
            state.doc_count_dirty.store(true, Ordering::Release);
            state.column_stats_dirty.store(true, Ordering::Release);
        }
        self.note_table_data_changed();
        Ok(())
    }

    fn analyze_table(
        &self,
        canonical_table_name: &str,
        t: &Arc<TableState>,
        persist: bool,
    ) -> StorageBackendResult<()> {
        let columns: Vec<String> = t
            .columns
            .read()
            .iter()
            .filter(|column| {
                !column.generated.as_ref().is_some_and(|generated| {
                    generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
                })
            })
            .map(|column| column.name.clone())
            .collect();
        let HierarchyAnalyzeInputs {
            row_count: n,
            values: mut col_values,
            null_counts: mut col_nulls,
        } = self.collect_hierarchy_analyze_inputs(canonical_table_name, &columns)?;

        let mut stats_out: BTreeMap<String, uqa_planner::ColumnStats> = BTreeMap::new();
        for col in &columns {
            let values = col_values.remove(col).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ANALYZE lost the value buffer for column `{col}`"
                ))
            })?;
            let null_count = col_nulls.remove(col).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ANALYZE lost the null counter for column `{col}`"
                ))
            })?;
            let distinct = distinct_count(&values)?;
            let comparable: Vec<&Value> = values
                .iter()
                .filter(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
                    )
                })
                .collect();
            let min_val = comparable.iter().min().map(|v| (*v).clone());
            let max_val = comparable.iter().max().map(|v| (*v).clone());

            let histogram = build_histogram(&comparable);
            let (mcv_values, mcv_frequencies) = build_mcv(&values, n);

            stats_out.insert(
                col.clone(),
                uqa_planner::ColumnStats {
                    distinct_count: distinct,
                    null_count,
                    min_value: min_val,
                    max_value: max_val,
                    row_count: n,
                    histogram,
                    mcv_values,
                    mcv_frequencies,
                },
            );
        }

        if persist && t.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                Self::persist_column_stats(catalog.as_ref(), canonical_table_name, &stats_out)?;
            }
        }
        *t.column_stats.write() = stats_out;
        t.column_stats_loaded.store(true, Ordering::Release);
        t.column_stats_dirty.store(false, Ordering::Release);
        Ok(())
    }

    fn collect_hierarchy_analyze_inputs(
        &self,
        canonical_table_name: &str,
        columns: &[String],
    ) -> StorageBackendResult<HierarchyAnalyzeInputs> {
        let mut col_values = columns
            .iter()
            .map(|column| (column.clone(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut col_nulls = columns
            .iter()
            .map(|column| (column.clone(), 0_u64))
            .collect::<BTreeMap<_, _>>();
        let members = self
            .hierarchy_scan_tables(canonical_table_name, true)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let mut n = 0_u64;
        for member_name in members {
            let member = self.try_table(&member_name)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ANALYZE hierarchy member `{member_name}` does not exist"
                ))
            })?;
            let snapshot = member.document_store.read().snapshot()?;
            let mut doc_ids: Vec<DocId> = snapshot.doc_ids()?;
            doc_ids.sort_unstable();
            n = n
                .checked_add(u64::try_from(doc_ids.len()).map_err(|_| {
                    StorageBackendError::Other("ANALYZE document count exceeds u64".into())
                })?)
                .ok_or_else(|| {
                    StorageBackendError::Other("ANALYZE hierarchy row count overflow".into())
                })?;
            let (member_values, member_nulls) =
                collect_analyze_values(snapshot.as_ref(), &doc_ids, columns)?;
            for column in columns {
                col_values
                    .get_mut(column)
                    .ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "ANALYZE lost the value buffer for column `{column}`"
                        ))
                    })?
                    .extend(member_values.get(column).cloned().unwrap_or_default());
                let null_count = member_nulls.get(column).copied().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "ANALYZE lost the null counter for column `{column}`"
                    ))
                })?;
                let total = col_nulls.get_mut(column).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "ANALYZE lost the null counter for column `{column}`"
                    ))
                })?;
                *total = total.checked_add(null_count).ok_or_else(|| {
                    StorageBackendError::Other("ANALYZE null count overflow".into())
                })?;
            }
        }
        Ok(HierarchyAnalyzeInputs {
            row_count: n,
            values: col_values,
            null_counts: col_nulls,
        })
    }

    fn persist_column_stats(
        catalog: &dyn CatalogFacade,
        table_name: &str,
        stats: &BTreeMap<String, uqa_planner::ColumnStats>,
    ) -> StorageBackendResult<()> {
        struct EncodedColumnStats {
            column_name: String,
            distinct_count: i64,
            null_count: i64,
            min_json: Option<String>,
            max_json: Option<String>,
            row_count: i64,
            histogram_json: String,
            mcv_values_json: String,
            mcv_frequencies_json: String,
        }

        let mut encoded = Vec::with_capacity(stats.len());
        for (col_name, cs) in stats {
            let min_json = cs
                .min_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let max_json = cs
                .max_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let histogram_json = serde_json::to_string(&cs.histogram)?;
            let mcv_values_json = serde_json::to_string(&cs.mcv_values)?;
            let mcv_frequencies_json = serde_json::to_string(&cs.mcv_frequencies)?;
            encoded.push(EncodedColumnStats {
                column_name: col_name.clone(),
                distinct_count: Self::u64_to_i64("distinct count", cs.distinct_count)?,
                null_count: Self::u64_to_i64("null count", cs.null_count)?,
                min_json,
                max_json,
                row_count: Self::u64_to_i64("row count", cs.row_count)?,
                histogram_json,
                mcv_values_json,
                mcv_frequencies_json,
            });
        }
        let rows = encoded
            .iter()
            .map(|stats| ColumnStatsInput {
                table_name,
                column_name: &stats.column_name,
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_json.as_deref(),
                max_value: stats.max_json.as_deref(),
                row_count: stats.row_count,
                histogram_json: &stats.histogram_json,
                mcv_values_json: &stats.mcv_values_json,
                mcv_frequencies_json: &stats.mcv_frequencies_json,
            })
            .collect::<Vec<_>>();
        catalog.replace_column_stats(table_name, &rows)
    }

    pub(super) fn u64_to_i64(kind: &str, value: u64) -> StorageBackendResult<i64> {
        i64::try_from(value).map_err(|_| {
            StorageBackendError::Other(format!(
                "ANALYZE {kind} {value} exceeds the persistent i64 range"
            ))
        })
    }

    /// Snapshot of the cardinality estimator's per-column statistics
    /// for `table`. Dirty stats are recomputed lazily so callers do not
    /// need to issue `ANALYZE` after every data change.
    pub fn column_stats(
        &self,
        table: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        self.try_column_stats(table)
    }

    pub fn try_column_stats(
        &self,
        table: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        // Lazy analysis must be linearizable with direct table mutations. A
        // stale scan must not publish `column_stats_dirty = false` after a
        // concurrent writer marked the table dirty. The gate is re-entrant
        // for optimizer calls already executing inside Engine::sql.
        let _statement = self.runtime.statement_gate.lock();
        self.synchronize_table_data()?;
        let canonical_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let t = self
            .try_table(&canonical_name)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        if t.column_stats_dirty.load(Ordering::Acquire) {
            self.analyze_table(&canonical_name, &t, false)?;
        }
        let stats = t.column_stats.read().clone();
        Ok(stats)
    }
}
