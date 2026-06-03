//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    value_to_f64_vec, value_to_usize, Arc, Engine, SQLError, TableState, TrainingExample,
    TrainingSet,
};

impl Engine {
    fn relation_lookup_candidates(&self, name: &str) -> Vec<String> {
        if name.contains('.') {
            return vec![name.to_string()];
        }
        let mut candidates = Vec::new();
        for schema in self.search_path.read().iter() {
            if schema == "pg_catalog" || schema == "information_schema" {
                continue;
            }
            if schema == "public" {
                candidates.push(name.to_string());
            } else {
                candidates.push(format!("{schema}.{name}"));
            }
        }
        if !candidates.iter().any(|candidate| candidate == name) {
            candidates.push(name.to_string());
        }
        candidates
    }

    pub(crate) fn relation_name_for_create(&self, name: &str) -> String {
        if name.contains('.') {
            return name.to_string();
        }
        let schema = self
            .search_path
            .read()
            .iter()
            .find(|schema| {
                schema.as_str() != "pg_catalog" && schema.as_str() != "information_schema"
            })
            .cloned()
            .unwrap_or_else(|| "public".to_string());
        if schema == "public" {
            name.to_string()
        } else {
            format!("{schema}.{name}")
        }
    }

    pub(crate) fn resolve_table_name(&self, name: &str) -> Option<String> {
        let tables = self.tables.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
    }

    pub(crate) fn resolve_view_name(&self, name: &str) -> Option<String> {
        let views = self.views.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| views.contains_key(candidate))
    }

    pub(crate) fn resolve_sequence_name(&self, name: &str) -> Option<String> {
        let sequences = self.sequences.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| sequences.contains_key(candidate))
    }

    pub(crate) fn resolve_foreign_table_name(&self, name: &str) -> Option<String> {
        let tables = self.foreign_tables.read();
        self.relation_lookup_candidates(name)
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
    }

    pub(crate) fn table(&self, name: &str) -> Option<Arc<TableState>> {
        let resolved = self.resolve_table_name(name)?;
        self.tables.read().get(&resolved).cloned()
    }

    pub(crate) fn training_set_from_table(
        &self,
        table: &str,
        features_field: &str,
        label_field: &str,
    ) -> Result<TrainingSet, SQLError> {
        let table_state = self
            .table(table)
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let store = table_state.document_store.read();
        let mut examples = Vec::new();
        for (doc_id, document) in store.iter_all() {
            let features = document.get(features_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{features_field}`"
                ))
            })?;
            let label = document.get(label_field).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} is missing `{label_field}`"
                ))
            })?;
            examples.push(TrainingExample {
                features: value_to_f64_vec(features).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{features_field}`: {e}"
                    ))
                })?,
                label: value_to_usize(label).map_err(|e| {
                    SQLError::TypeMismatch(format!(
                        "deep_learn table {table:?} row {doc_id} `{label_field}`: {e}"
                    ))
                })?,
            });
        }
        Ok(TrainingSet {
            examples,
            class_count: None,
        })
    }
}
