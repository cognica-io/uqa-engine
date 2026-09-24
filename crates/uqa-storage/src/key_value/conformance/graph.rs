//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same graph-cache schedules run against common memory records and durable providers.

use std::sync::Arc;

use crate::{CatalogFacade, KeyValueCatalog, KeyValueStore, StorageBackendResult};

use super::expect;

const GRAPH: &str = "graph\0日本語";
const INDEX: &str = "paths\0日本語";

fn build(catalog: &KeyValueCatalog, index: &str, graph: &str) -> StorageBackendResult<()> {
    catalog.save_path_index(index, "[]")?;
    catalog.finish_path_index_data(index, graph, "[]")?;
    expect(
        catalog.path_index_data_is_current(index, "[]")?,
        "completed graph cache build",
    )
}

/// Verify graph-derived state on two independent logical sessions over a fresh disposable database. Fixtures remain available for `verify_graph_cache_reopen` after all handles close.
pub fn verify_graph_cache_concurrency(
    a: Arc<dyn KeyValueStore>,
    b: Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let first = KeyValueCatalog::new(a.clone());
    let second = KeyValueCatalog::new(b.clone());
    first.save_named_graph(GRAPH)?;
    for id in 1..=2 {
        first.save_vertex(id, "node", "{}")?;
        first.save_graph_membership("vertex", id, GRAPH)?;
    }
    build(&first, INDEX, GRAPH)?;
    a.begin_transaction()?;
    first.save_vertex(1, "node", "{\"value\":1}")?;
    b.begin_transaction()?;
    second.save_vertex(2, "node", "{\"value\":2}")?;
    b.commit_transaction()?;
    expect(
        a.in_transaction(),
        "first graph writer remains private after second commit",
    )?;
    a.commit_transaction()?;
    expect(
        !first.path_index_data_is_current(INDEX, "[]")?,
        "independent graph writers merge invalidation",
    )?;
    expect(
        first.graph_vertex(1)?.unwrap().properties_json == "{\"value\":1}",
        "first graph source survives",
    )?;
    expect(
        first.graph_vertex(2)?.unwrap().properties_json == "{\"value\":2}",
        "second graph source survives",
    )?;

    a.begin_transaction()?;
    first.save_vertex(1, "node", "{\"value\":3}")?;
    second.save_named_graph("late")?;
    second.save_graph_membership("vertex", 1, "late")?;
    build(&second, "late-source", GRAPH)?;
    build(&second, "late-member", "late")?;
    a.commit_transaction()?;
    for index in ["late-source", "late-member"] {
        expect(
            !second.path_index_data_is_current(index, "[]")?,
            "late graph dependency invalidated",
        )?;
    }

    a.begin_transaction()?;
    first.finish_path_index_data(INDEX, GRAPH, "[]")?;
    second.save_vertex(2, "node", "{\"value\":4}")?;
    a.commit_transaction()?;
    expect(
        !first.path_index_data_is_current(INDEX, "[]")?,
        "stale graph build is not published as current",
    )?;

    a.begin_transaction()?;
    first.save_vertex(1, "node", "{\"value\":5}")?;
    first.finish_path_index_data(INDEX, GRAPH, "[]")?;
    a.savepoint("rebuilt")?;
    first.save_vertex(2, "node", "{\"value\":6}")?;
    expect(
        !first.path_index_data_is_current(INDEX, "[]")?,
        "private mutation invalidates private build",
    )?;
    a.rollback_to_savepoint("rebuilt")?;
    expect(
        first.path_index_data_is_current(INDEX, "[]")?,
        "savepoint restores private build",
    )?;
    second.save_vertex(3, "outside", "{}")?;
    a.commit_transaction()?;

    first.save_named_graph("moved-from")?;
    first.save_named_graph("moved-to")?;
    first.save_vertex(4, "node", "{}")?;
    first.save_graph_membership("vertex", 4, "moved-from")?;
    build(&first, "moved", "moved-from")?;
    a.begin_transaction()?;
    first.save_vertex(4, "node", "{\"value\":4}")?;
    second.finish_path_index_data("moved", "moved-to", "[]")?;
    a.commit_transaction()?;
    verify_graph_cache_reopen(a)?;
    verify_graph_cache_reopen(b)
}

/// Verify the final state produced by `verify_graph_cache_concurrency`, including after reopening the durable provider.
pub fn verify_graph_cache_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store);
    for (id, expected) in [
        (1, "{\"value\":5}"),
        (2, "{\"value\":4}"),
        (3, "{}"),
        (4, "{\"value\":4}"),
    ] {
        expect(
            catalog
                .graph_vertex(id)?
                .is_some_and(|row| row.properties_json == expected),
            "graph source after reopen",
        )?;
    }
    expect(
        catalog.path_index_data_is_current(INDEX, "[]")?,
        "private rebuild survives unrelated commit and reopen",
    )?;
    expect(
        catalog.path_index_data_is_current("moved", "[]")?,
        "old graph writer preserves a cache reassigned to another graph",
    )?;
    for index in ["late-source", "late-member"] {
        expect(
            !catalog.path_index_data_is_current(index, "[]")?,
            "late dependency remains invalid after reopen",
        )?;
    }
    Ok(())
}
