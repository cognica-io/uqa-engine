//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct only the physical projection of a private, format-zero native image.

use super::{
    cache_families, check_mapping, create_provider_tables, insert_format,
    install_equality_projection, install_guards, invalid, occurrence_accelerators,
    prepare_catalog_sources, present, schema, validate_format, Connection, DatabaseId, Family,
    NativeRecordNamespace, PhysicalResult, StorageReadControl, CURRENT_VERSION,
};

pub(in crate::mvcc::native) fn initialize(
    connection: &Connection,
    namespace: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let _permit = crate::mvcc::admission::permit(connection, control)?;
    let transaction = crate::mvcc::admission::begin(connection, control)?;
    let private: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_metadata WHERE singleton = 1 AND format = 0 AND mapping = 0 AND restore_target IS NOT NULL)", [], |row| row.get(0))?;
    if !private || present(&transaction)? || namespace.as_bytes() == [0; 16] {
        return Err(invalid("native image projection requires a fresh private import").into());
    }
    // Reuse the catalog's current schema construction. Its fixed bootstrap rows are not image contents and are removed before any captured row is materialized.
    prepare_catalog_sources(&transaction, control)?;
    create_provider_tables(&transaction)?;
    occurrence_accelerators::create(&transaction)?;
    install_equality_projection(&transaction)?;
    for table in Family::all()
        .map(|family| family.layout().table)
        .chain(schema::TABLES.iter().map(|(table, _)| *table))
        .chain([schema::image_binding::IMAGE.0])
    {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let (name, _) = crate::Catalog::cache_revision_trigger(table, false, event);
            transaction.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
        }
    }
    for family in super::super::projection::ORDER.into_iter().rev() {
        transaction.execute(&format!("DELETE FROM {}", family.layout().table), [])?;
    }
    insert_format(&transaction, namespace)?;
    crate::mvcc::admission::commit(transaction, control)
}

pub(in crate::mvcc::native) fn complete(
    connection: &Connection,
    namespace: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let _permit = crate::mvcc::admission::permit(connection, control)?;
    let transaction = crate::mvcc::admission::begin(connection, control)?;
    for family in cache_families(CURRENT_VERSION) {
        let layout = family.layout();
        for event in ["INSERT", "DELETE", "UPDATE"] {
            transaction.execute_batch(
                &crate::Catalog::cache_revision_trigger(
                    layout.table,
                    layout.columns.contains(&"table_name"),
                    event,
                )
                .1,
            )?;
        }
    }
    install_guards(&transaction)?;
    validate_format(&transaction, CURRENT_VERSION)?;
    check_mapping(&transaction, Some(NativeRecordNamespace(namespace)))?;
    crate::mvcc::admission::commit(transaction, control)
}
