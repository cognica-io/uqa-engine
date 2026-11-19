//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::graph_oids::GraphCatalogOids;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};
const RECORD: &str = r#"{"namespace":20001,"label_sequence":20002,"labels":{"17":{"sequence":20003,"relation":{"relation":20004},"id_default":20007,"properties_default":20008,"not_null":{"Exact.Column":20009},"toast_table":20011,"toast_index":20012,"trigger":20017}}}"#;

fn fixture(count: usize) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    for index in (0..count).rev() {
        catalog
            .set_metadata(
                &format!("{GRAPH_CATALOG_OIDS_METADATA_PREFIX}graph_{index:04}_한글"),
                RECORD,
            )
            .unwrap();
    }
    // Metadata may predate graph membership; loading does not invent a cross-registry predicate.
    catalog
        .set_metadata(GRAPH_CATALOG_OIDS_METADATA_PREFIX, RECORD)
        .unwrap();
    catalog
        .set_metadata("irrelevant", "invalid graph JSON")
        .unwrap();
    catalog
}

fn finish(
    input: &mut GraphOidCatalogInput,
    window: usize,
) -> StorageBackendResult<(GraphOidRegistry, usize)> {
    for count in 1..1_000_000 {
        if let Some(value) = input.advance(window)? {
            return Ok((value, count));
        }
    }
    panic!("graph OID input did not progress")
}

#[test]
fn persistent_graph_oid_input_preserves_ordered_native_roots_and_later_mutation_ownership() {
    for count in [0, 3, 64] {
        let catalog = fixture(count);
        let original = catalog.metadata_with_prefix("").unwrap();
        let expected = crate::catalog::graph_oids::load(catalog.as_ref()).unwrap();
        let mut windows = Vec::new();
        for window in [1, 7, 64] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let mut input =
                GraphOidCatalogInput::new(Arc::clone(&catalog), LIMITS, &mutations, &control)
                    .unwrap();
            let (mut actual, count) = finish(&mut input, window).unwrap();
            windows.push(count);
            assert_eq!(actual, expected);
            assert!(input.advance(1).is_err());
            let retained = actual
                .capture(&ProductionControl::new(
                    control.memory(),
                    control.cancellation(),
                    control.cancellation(),
                ))
                .unwrap();
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            control.cancellation().cancel();
            actual
                .insert(
                    "later".into(),
                    serde_json::from_str::<GraphCatalogOids>(RECORD).unwrap(),
                )
                .unwrap();
            assert!(retained.get("later").is_none());
            assert!(actual.contains_key("later"));
            drop((actual, retained, input));
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
        assert!(windows[0] > windows[1] && windows[1] > windows[2]);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
    }
}

#[test]
fn persistent_graph_oid_input_finishes_physical_validation_before_semantic_records() {
    let catalog = fixture(0);
    catalog
        .set_metadata(GRAPH_CATALOG_OIDS_METADATA_PREFIX, "not JSON")
        .unwrap();
    catalog
        .set_metadata(
            &format!("{GRAPH_CATALOG_OIDS_METADATA_PREFIX}z"),
            &"x".repeat(1024),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = GraphOidCatalogInput::new(
        Arc::clone(&catalog),
        CatalogInputLimits {
            maximum_value_bytes: 512,
            ..LIMITS
        },
        control.memory(),
        &control,
    )
    .unwrap();
    let error = finish(&mut input, 1)
        .expect_err("physical size failure")
        .to_string();
    assert!(
        !error.contains("invalid stored graph catalog OIDs"),
        "{error}"
    );
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    catalog
        .delete_metadata(&format!("{GRAPH_CATALOG_OIDS_METADATA_PREFIX}z"))
        .unwrap();
    let mut input = GraphOidCatalogInput::new(catalog, LIMITS, control.memory(), &control).unwrap();
    let error = finish(&mut input, 7).unwrap_err();
    assert!(matches!(error, StorageBackendError::Serde(_)));
    assert!(error
        .to_string()
        .contains("invalid stored graph catalog OIDs"));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn persistent_graph_oid_input_retires_physical_and_nested_json_prefixes_without_publication() {
    let catalog = fixture(16);
    let original = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 16, 128, 512] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let mut input =
                GraphOidCatalogInput::new(Arc::clone(&catalog), LIMITS, &mutations, &control)
                    .unwrap();
            for _ in 0..cut {
                assert!(input.advance(1).unwrap().is_none());
            }
            let occupied = if terminal == "memory" {
                Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                )
            } else {
                None
            };
            match terminal {
                "cancel" => control.cancellation().cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire graph OID restoration");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                assert!(finish(&mut input, usize::from(terminal != "zero")).is_err());
                assert!(input.advance(1).is_err());
            }
            drop((input, occupied));
            assert_eq!(control.memory().used(), 0, "{terminal}/{cut}");
            assert_eq!(mutations.used(), 0);
        }
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
}
