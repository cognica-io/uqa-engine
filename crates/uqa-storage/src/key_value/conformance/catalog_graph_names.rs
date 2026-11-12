//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq};
use crate::{
    catalog::input::{CatalogInput, CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    PersistentStorageSession, StorageBackendResult,
};
use uqa_core::memory::Budgeted;

fn finish(input: &mut CatalogInput<String>) -> StorageBackendResult<Vec<Budgeted<String>>> {
    let mut names = Vec::new();
    for _ in 0..65536 {
        match input.advance()? {
            CatalogInputStep::Pending => (),
            CatalogInputStep::Row(name) => names.push(name),
            CatalogInputStep::Complete => return Ok(names),
        }
    }
    Err(crate::StorageBackendError::Other(
        "graph-name input stopped progressing".into(),
    ))
}

/// Verify complete physical graph names against original catalog membership while the originating private view is rolled back and released.
pub fn verify_catalog_graph_name_input(
    source: PersistentStorageSession,
) -> StorageBackendResult<()> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 65536,
    };
    source.catalog.initialize_storage()?;
    for name in ["z", "a_longer", "한글\0graph", "removed"] {
        source.catalog.save_named_graph(name)?;
    }
    source.backend.begin_transaction()?;
    source.catalog.save_named_graph("private")?;
    source.catalog.drop_named_graph("removed")?;
    let mut expected = source.catalog.load_named_graphs()?;
    expected.sort();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = source.catalog.begin_graph_name_input(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.drop_named_graph("z")?;
    later.catalog.save_named_graph("later")?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "graph-name input does not retain the source catalog handle",
    )?;
    let mut names = finish(&mut input)?;
    names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    expect(
        names.iter().map(|name| &**name).eq(expected.iter()),
        "graph-name input preserves original private membership and exact names",
    )?;
    expect(
        matches!(input.advance()?, CatalogInputStep::Complete),
        "completed graph names do not reopen persistence",
    )?;
    control.cancellation().cancel();
    expect(
        names.iter().map(|name| &**name).eq(expected.iter()),
        "completed names survive original cancellation",
    )?;
    drop((names, input));
    expect_eq(
        &control.memory().used(),
        &0,
        "complete graph-name retirement releases original allowance",
    )?;
    verify_retirement(&later, limits)
}

fn verify_retirement(
    later: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for terminal in ["cancel", "memory", "encoded", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = later.catalog.begin_graph_name_input(limits, &control)?;
        drop(input.advance()?);
        let occupied = if terminal == "memory" {
            Some(control.memory().reserve(control.memory().available())?)
        } else {
            None
        };
        match terminal {
            "cancel" => {
                control.cancellation().cancel();
                expect(input.advance().is_err(), "cancelled names fail")?;
            }
            "memory" => {
                expect(
                    finish(&mut input).is_err(),
                    "names retain original admission",
                )?;
            }
            "encoded" => {
                drop(input);
                input = later.catalog.begin_graph_name_input(
                    CatalogInputLimits {
                        maximum_key_bytes: 128,
                        ..limits
                    },
                    &control,
                )?;
                later.catalog.save_named_graph(&"wide".repeat(100))?;
                // The already selected view does not acquire a later oversized name.
                drop(finish(&mut input)?);
                let mut selected = later.catalog.begin_graph_name_input(
                    CatalogInputLimits {
                        maximum_key_bytes: 128,
                        ..limits
                    },
                    &control,
                )?;
                expect(
                    finish(&mut selected).is_err(),
                    "selected oversized names respect encoded key limits",
                )?;
                later.catalog.drop_named_graph(&"wide".repeat(100))?;
            }
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("retire graph names");
                }));
                drop(occupied);
                expect_eq(
                    &control.memory().used(),
                    &0,
                    "unwinding retires graph-name input",
                )?;
                continue;
            }
            "drop" => (),
            _ => unreachable!(),
        }
        drop((input, occupied));
        expect_eq(
            &control.memory().used(),
            &0,
            "partial graph-name input releases the selected view and cursor",
        )?;
    }
    Ok(())
}
