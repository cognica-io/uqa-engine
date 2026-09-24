//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial-transaction normalization of typed catalog vectors and their dependent indexes.

use std::collections::BTreeSet;
use uqa_core::memory::{MemoryBudget, ProductionControl};
use uqa_sql::{
    assignment::conversion::{
        contains_legacy_vectors, normalize_legacy_vector_carrier_with_control,
    },
    ast::{ColumnDef, IndexKey},
};
use uqa_storage::{
    document_store::{read_document_ids, read_stored_documents},
    read_control::StorageReadControl,
    CatalogFacade, CatalogIndexRow, PersistentStorageBackend, StorageBackendError,
    StorageBackendResult,
};

const VERSION_KEY: &str = "sql_legacy_vector_carrier_version";

/// Engine lends retained table caches and the existing physical-index restoration interface.
pub trait ValueRestorationSession {
    fn index_build_context(&self) -> crate::schema::indexes::IndexBuildContext<'_>;
    fn rebuild_value_indexes_and_refresh_statistics(&self, table: &str)
        -> StorageBackendResult<()>;
}

/// Join the caller's initial catalog transaction. The version marker is published only after every row, dependent index and cached statistic has been converted; the caller rolls back all private changes on failure.
pub fn normalize_legacy_vectors(
    catalog: &dyn CatalogFacade,
    backend: &dyn PersistentStorageBackend,
    session: &dyn ValueRestorationSession,
) -> StorageBackendResult<()> {
    if !backend.in_transaction() {
        return Err(invalid(
            "legacy vector restoration requires the initial transaction",
        ));
    }
    match catalog.get_metadata(VERSION_KEY)?.as_deref() {
        Some("1") => return Ok(()),
        None => {}
        Some(_) => return Err(invalid("unsupported legacy vector carrier version")),
    }
    let control = backend.retention_control().unwrap_or_else(|| {
        StorageReadControl::new(
            &MemoryBudget::new(usize::MAX),
            &backend.write_cancellation().unwrap_or_default(),
        )
    });
    let indexes = catalog.load_catalog_indexes()?;
    let mut affected = expression_index_tables(&indexes)?;
    for table in catalog.load_tables()? {
        control.check()?;
        if table.columns_json.is_empty() {
            continue;
        }
        let columns: Vec<ColumnDef> = serde_json::from_str(&table.columns_json)?;
        let columns = columns
            .into_iter()
            .filter(|column| contains_legacy_vectors(&column.ty))
            .collect::<Vec<_>>();
        if columns.is_empty() {
            continue;
        }
        let name = table.relation.qualified_name();
        normalize_table(backend, &name, &columns, &control)?;
        affected.insert(name);
    }
    for index in &indexes {
        if affected.contains(&canonical_table_name(&index.table_name)?)
            && super::index::index_definition(index)?.unique
        {
            control.check()?;
            let declaration = uqa_sql::catalog::index::stored::declaration(index)?;
            crate::schema::indexes::validate_unique_index(
                &session.index_build_context(),
                &declaration,
                &index.relation.name,
            )
            .map_err(|error| StorageBackendError::backend("restored index validation", error))?;
        }
    }
    for table in affected {
        control.check()?;
        for field in backend.btree_index_fields(&table)? {
            backend.drop_btree_index(&table, &field)?;
        }
        catalog.delete_column_stats(&table)?;
        session.rebuild_value_indexes_and_refresh_statistics(&table)?;
    }
    catalog.set_metadata(VERSION_KEY, "1")
}

fn expression_index_tables(indexes: &[CatalogIndexRow]) -> StorageBackendResult<BTreeSet<String>> {
    let mut tables = BTreeSet::new();
    for row in indexes {
        if !row.index_type.eq_ignore_ascii_case("btree") {
            continue;
        }
        let definition = super::index::index_definition(row)?;
        let unknown_expression_type = if definition.key_types.is_empty() {
            serde_json::from_str::<Vec<IndexKey>>(&row.columns_json)?
                .iter()
                .any(|key| matches!(key, IndexKey::Expression(_)))
        } else {
            false
        };
        if unknown_expression_type || definition.key_types.iter().any(contains_legacy_vectors) {
            tables.insert(canonical_table_name(&row.table_name)?);
        }
    }
    Ok(tables)
}

fn canonical_table_name(name: &str) -> StorageBackendResult<String> {
    uqa_core::RelationIdentity::from_legacy_name(name)
        .map(|relation| relation.qualified_name())
        .map_err(StorageBackendError::Other)
}

fn normalize_table(
    backend: &dyn PersistentStorageBackend,
    table: &str,
    columns: &[ColumnDef],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut documents = backend.document_store(table);
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let mut after = None;
    loop {
        let ids = read_document_ids(documents.as_ref(), after, 256, control)?;
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids.iter().copied() {
            let mut page = read_stored_documents(documents.as_ref(), &[id], control)?;
            let row = page
                .pop()
                .flatten()
                .ok_or_else(|| invalid("legacy vector restoration lost a document"))?;
            drop(page);
            let (row, reservation) = row.into_budgeted(control)?.into_parts();
            let mut memory = reservation;
            let mut row = row;
            let mut changed = false;
            for column in columns {
                let Some(value) = row.fields_mut().get_mut(&column.name) else {
                    continue;
                };
                if let Some(replacement) =
                    normalize_legacy_vector_carrier_with_control(value, &column.ty, &production)
                        .map_err(|error| {
                            StorageBackendError::backend("legacy vector restoration", error)
                        })?
                {
                    let (replacement, reservation) = replacement.into_parts();
                    if let Some(reservation) = reservation {
                        memory.absorb(reservation);
                    }
                    *value = replacement;
                    changed = true;
                }
            }
            if changed {
                documents.put_stored(id, row)?;
            } else {
                drop(row);
            }
            drop(memory);
        }
        after = ids.last().copied();
    }
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
