//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real catalog ownership, definition races and private undo on disposable providers.

use std::sync::Arc;

use super::{expect, expect_eq};
use crate::key_value::{KeyValueDiskANNCanonical, RetainedDiskANNCanonical};
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{
    CatalogFacade, CatalogIndexRow, KeyValueCatalog, KeyValueStore, RelationIdentity,
    RelationSecurityRow, StorageBackendResult, TableSchema, VectorFieldSchema,
};

const TABLE: &str = "public.diskann_binding";
const FIELD: &str = "vector";
const MARKER: &[u8] = b"diskann-binding-guarded-write";

mod identity;
mod live;
mod pruning;
mod publication;
mod runtime;
pub use runtime::verify_diskann_runtime_adoption_conflicts;
mod selection;
pub use identity::{verify_diskann_catalog_identity, verify_diskann_catalog_identity_reopen};
pub use live::{verify_diskann_live_reopen, verify_diskann_live_writes};
pub use pruning::{verify_diskann_pruning, verify_diskann_pruning_reopen};
pub use publication::{verify_diskann_publication, verify_diskann_publication_reopen};
pub use runtime::{
    diskann_runtime_fixture_options, verify_diskann_runtime_lifecycle,
    verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_reclamation,
    verify_diskann_runtime_reopen, verify_diskann_runtime_retirement,
    verify_diskann_runtime_retirement_reopen,
};
pub use selection::{verify_diskann_query_reopen, verify_diskann_query_views};

fn definition() -> StorageBackendResult<CatalogIndexRow> {
    Ok(CatalogIndexRow {
        relation: RelationIdentity::new("public", "diskann_binding_idx"),
        index_type: "diskann".into(),
        table_name: TABLE.into(),
        columns_json: "[\"vector\"]".into(),
        parameters_json: serde_json::to_string(
            &DiskANNIndexParams::for_dimensions(2)?.to_catalog_map(2)?,
        )
        .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?,
        // The storage owner binds the actual record revision, including legacy opaque definitions.
        definition_json: None,
    })
}

fn setup(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<KeyValueDiskANNCanonical> {
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public")?;
    catalog.save_table(&TableSchema {
        relation: RelationIdentity::new("public", "diskann_binding"),
        security: RelationSecurityRow::legacy("owner"),
        object_id: [79; 16],
        storage_generation: [80; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![VectorFieldSchema {
            field: FIELD.into(),
            dimensions: 2,
        }],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    })?;
    catalog.save_catalog_index_row(&definition()?)?;
    KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)
}

fn guard(
    store: &Arc<dyn KeyValueStore>,
    source: &RetainedDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.with_mutation(&mut |read, batch| {
        source.require_current_index(read, batch, control)?;
        batch.put(MARKER, b"accepted")
    })
}

/// Verify that a build can retain actual catalog provenance without fencing normal data changes or trusting equal catalog bytes. Both arguments must be independent disposable databases.
pub fn verify_diskann_catalog_binding(
    store: &Arc<dyn KeyValueStore>,
    foreign: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    let _foreign = setup(foreign)?;
    let row = definition()?;
    let version = canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    let source = canonical.retain_for_index(&row.relation, &control)?;
    expect_eq(
        &source.index_parameters(),
        &Some(DiskANNIndexParams::for_dimensions(2)?),
        "captured persisted parameters",
    )?;
    store.put(b"unrelated-binding-commit", b"kept")?;
    canonical.replace(1, &[vec![0.0, 1.0]], &control)?;
    expect_eq(
        &source.origin(1, &control)?,
        &Some(version),
        "bound input keeps original origin",
    )?;
    guard(store, &source, &control)?;
    expect(
        guard(foreign, &source, &control).is_err(),
        "equal foreign catalog is not the same provider",
    )?;
    expect(
        foreign.get(MARKER)?.is_none(),
        "foreign guard failure leaves no write",
    )?;
    let plain = canonical.retain(&control)?;
    expect(
        guard(store, &plain, &control).is_err(),
        "unbound source cannot acquire a live binding",
    )?;
    invalid_definitions(store, &canonical, &control)?;
    private_definitions(store, &canonical, &control)?;
    stale_definitions(store, &canonical, &control)?;
    publication_race(store, &canonical, &control)?;
    let source = canonical.retain_for_index(&row.relation, &control)?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        canonical.retain_for_index(&row.relation, &tiny).is_err(),
        "binding capture retains its allowance",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed binding capture releases workspace",
    )?;
    control.cancellation().cancel();
    expect(
        guard(store, &source, &StorageReadControl::with_limit(8192)).is_err(),
        "binding preserves captured cancellation",
    )
}

fn invalid_definitions(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let row = definition()?;
    for kind in 0..4 {
        let mut changed = row.clone();
        match kind {
            0 => changed.index_type = "hnsw".into(),
            1 => changed.columns_json = "[\"other\"]".into(),
            2 => changed.columns_json = "[\"vector\",\"vector\"]".into(),
            _ => changed.parameters_json = "{}".into(),
        }
        catalog.save_catalog_index_row(&changed)?;
        expect(
            canonical.retain_for_index(&row.relation, control).is_err(),
            "foreign field, method or incomplete parameters rejected",
        )?;
    }
    catalog.save_catalog_index_row(&row)?;
    let wrong = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 3)?;
    expect(
        wrong.retain_for_index(&row.relation, control).is_err(),
        "dimensions must match real table",
    )?;
    catalog.drop_catalog_index(&row.relation)?;
    expect(
        canonical.retain_for_index(&row.relation, control).is_err(),
        "missing index rejected",
    )?;
    catalog.save_catalog_index_row(&row)
}

fn private_definitions(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let row = definition()?;
    store.begin_transaction()?;
    store.savepoint("binding")?;
    catalog.save_catalog_index_row(&row)?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    guard(store, &source, control)?;
    store.put(b"unrelated-private-binding", b"kept")?;
    guard(store, &source, control)?;
    store.rollback_to_savepoint("binding")?;
    expect(
        guard(store, &source, control).is_err(),
        "undone definition is not current",
    )?;
    catalog.save_catalog_index_row(&row)?;
    expect(
        guard(store, &source, control).is_err(),
        "equal definition in a new undo branch differs",
    )?;
    store.rollback_transaction()
}

fn stale_definitions(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let row = definition()?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    catalog.drop_catalog_index(&row.relation)?;
    catalog.save_catalog_index_row(&row)?;
    expect(
        guard(store, &source, control).is_err(),
        "identical recreated index invalidates capture",
    )?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    let mut schema = catalog
        .load_tables()?
        .into_iter()
        .find(|item| item.relation.qualified_name() == TABLE)
        .ok_or_else(|| crate::StorageBackendError::Other("missing fixture table".into()))?;
    schema.storage_generation = [81; 16];
    catalog.save_table(&schema)?;
    expect(
        guard(store, &source, control).is_err(),
        "truncate owner invalidates capture",
    )?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    catalog.drop_table_and_data(TABLE)?;
    setup(store)?;
    expect(
        guard(store, &source, control).is_err(),
        "recreated table invalidates even reused fixture IDs",
    )
}

fn publication_race(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let peer = store.open_session()?;
    let row = definition()?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    store.delete(MARKER)?;
    store.begin_transaction()?;
    guard(store, &source, control)?;
    KeyValueCatalog::new(peer.clone()).save_catalog_index_row(&row)?;
    expect(
        store.commit_transaction().is_err(),
        "definition race rejected at actual commit",
    )?;
    store.rollback_transaction()?;
    expect(
        store.get(MARKER)?.is_none(),
        "failed publication guard is atomic",
    )?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    store.begin_transaction()?;
    guard(store, &source, control)?;
    KeyValueDiskANNCanonical::new(peer, TABLE, FIELD, 2)?.replace(99, &[], control)?;
    store.commit_transaction()?;
    expect(
        store.get(MARKER)?.is_some(),
        "disjoint data commit does not invalidate build binding",
    )
}
