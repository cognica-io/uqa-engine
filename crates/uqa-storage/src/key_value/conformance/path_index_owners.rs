//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq};
use crate::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    PersistentStorageSession, StorageBackendError, StorageBackendResult,
};
use uqa_core::memory::MemoryError;

/// Check exact encoded point limits, retention of invalidated owners and the current transaction's private state.
pub fn verify_controlled_path_index_owner_reads(
    session: PersistentStorageSession,
    stores_full_definition: bool,
) -> StorageBackendResult<CatalogInputLimits> {
    let index = "owner\0경로";
    let graph = "한글".repeat(2048);
    let definition = format!("[[\"{}\"]]", "edge".repeat(8192));
    let catalog = &session.catalog;
    catalog.initialize_storage()?;
    catalog.save_named_graph(&graph)?;
    catalog.save_path_index(index, &definition)?;
    catalog.finish_path_index_data(index, &graph, &definition)?;

    let exact = verify_limits(
        catalog.as_ref(),
        index,
        graph.len(),
        definition.len(),
        stores_full_definition,
    )?;
    let control = StorageReadControl::with_limit(1 << 20);
    session.backend.begin_transaction()?;
    catalog.save_path_index(index, "[]")?;
    let retained = catalog
        .path_index_graph_with_control(index, exact, &control)?
        .ok_or_else(|| StorageBackendError::Other("invalidated path owner was lost".into()))?;
    catalog.clear_path_index_data(index)?;
    expect(
        catalog
            .path_index_graph_with_control(index, exact, &control)?
            .is_none(),
        "private removal hides the owner",
    )?;
    session.backend.rollback_transaction()?;
    expect_eq(
        &catalog
            .path_index_graph_with_control(index, exact, &control)?
            .as_deref(),
        &Some(&graph),
        "rollback restores the original owner",
    )?;
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    expect(
        matches!(
            catalog.path_index_graph_with_control(index, exact, &cancelled),
            Err(StorageBackendError::Cancelled(_))
        ),
        "cancel before provider access",
    )?;
    drop(session);
    expect_eq(
        &retained.as_str(),
        &graph.as_str(),
        "original UTF-8 survives storage retirement",
    )?;
    expect(
        retained.budget().shares_allowance(control.memory()),
        "owner carries the caller's admission",
    )?;
    expect_eq(
        &control.memory().used(),
        &retained.capacity(),
        "only the returned owner survives",
    )?;
    drop(retained);
    expect_eq(
        &control.memory().used(),
        &0,
        "dropping the graph releases the original allowance",
    )?;
    Ok(exact)
}

fn verify_limits(
    catalog: &dyn CatalogFacade,
    index: &str,
    graph_bytes: usize,
    definition_bytes: usize,
    stores_full_definition: bool,
) -> StorageBackendResult<CatalogInputLimits> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 1 << 20,
    };
    let small = StorageReadControl::with_limit(8192);
    let required = |key_limit, value_limit| match catalog.path_index_graph_with_control(
        index,
        CatalogInputLimits {
            maximum_key_bytes: key_limit,
            maximum_value_bytes: value_limit,
        },
        &small,
    ) {
        Err(StorageBackendError::Memory(MemoryError::Limit { required, limit }))
            if limit == key_limit || limit == value_limit =>
        {
            Ok(required)
        }
        _ => Err(StorageBackendError::Other(
            "expected a physical path-index point limit".into(),
        )),
    };
    let key_bytes = required(index.len(), limits.maximum_value_bytes)?;
    let value_bytes = required(limits.maximum_key_bytes, 1)?;
    expect(
        if stores_full_definition {
            key_bytes > index.len() + 6 && value_bytes > definition_bytes + graph_bytes
        } else {
            key_bytes == index.len() + 6 && value_bytes == graph_bytes
        },
        "point bounds measure the provider's physical identity and owner record",
    )?;
    expect_eq(
        &required(key_bytes - 1, limits.maximum_value_bytes)?,
        &key_bytes,
        "one below the exact key limit",
    )?;
    expect_eq(
        &required(key_bytes, value_bytes - 1)?,
        &value_bytes,
        "one below the exact row limit",
    )?;
    expect_eq(
        &small.memory().used(),
        &0,
        "rejected owners retire storage scratch",
    )?;
    expect(
        small.memory().peak() < graph_bytes,
        "size rejection precedes graph materialization",
    )?;

    Ok(CatalogInputLimits {
        maximum_key_bytes: key_bytes,
        maximum_value_bytes: value_bytes,
    })
}
