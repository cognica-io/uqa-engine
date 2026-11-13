//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder};
use uqa_storage::{catalog::input::CatalogInputLimits, read_control::StorageReadControl};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 1 << 20,
    maximum_value_bytes: 4 << 20,
};

impl GinAnalyzerInputTable for Table {
    async fn register_field_from(
        &self,
        field: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        let next = self
            .fields()
            .with_appended_name_from(field, input)
            .await
            .map_err(|error| StorageBackendError::backend("test GIN fields", error))?;
        input.checkpoint().await?;
        *self.fields.lock() = next;
        Ok(())
    }
}
impl GinAnalyzerInputTables for Arc<Tables> {
    type Table = Table;
    async fn table_from(
        &self,
        name: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<Table>> {
        input.checkpoint().await?;
        self.table(name)
    }
}

fn catalog(rows: &[CatalogIndexRow]) -> uqa_storage::key_value::KeyValueCatalog {
    let catalog = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory()
        .unwrap()
        .catalog();
    catalog.initialize_storage().unwrap();
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    let names = rows
        .iter()
        .map(|row| RelationIdentity::from_legacy_name(&row.table_name).unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    for (index, relation) in names.into_iter().enumerate() {
        let identity = (index as u128 + 1).to_be_bytes();
        catalog
            .save_table(&uqa_storage::TableSchema {
                relation,
                security: uqa_storage::RelationSecurityRow::bootstrap(),
                object_id: identity,
                storage_generation: identity,
                analyzer_json: "{}".into(),
                fts_fields: Vec::new(),
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: "{}".into(),
            })
            .unwrap();
    }
    for (index, row) in rows.iter().enumerate() {
        let mut row = row.clone();
        row.relation = RelationIdentity::new("public", format!("index_{index:04}"));
        catalog.save_catalog_index_row(&row).unwrap();
    }
    catalog
}

fn finish(input: &mut GinAnalyzerCatalogInput) -> StorageBackendResult<RetainedGinAnalyzerOwners> {
    for _ in 0..1_000_000 {
        if let Some(owners) = input.advance(1)? {
            return Ok(owners);
        }
    }
    panic!("GIN input stopped progressing")
}

fn observe(
    owners: RetainedGinAnalyzerOwners,
    expected: &GinAnalyzerOwners,
    control: &StorageReadControl,
) {
    let expected = expected.clone();
    let mut input = ImageDecoder::<(), StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            for (key, name) in expected {
                assert_eq!(
                    owners.find_from(&key, &mut input).await?,
                    Some(name.as_str())
                );
            }
            assert!(owners
                .find_from(&("public.docs".into(), "absent".into()), &mut input)
                .await?
                .is_none());
            Ok(())
        },
    )
    .unwrap();
    loop {
        if input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
            .is_some()
        {
            break;
        }
    }
}

#[test]
fn gin_input_preserves_complete_key_parameter_and_field_observations() {
    let keys = r#"["body",{"Column":"ignored_expression"},{"WindowCall":{"name":"count","args":[],"spec":{"partition_by":[],"order_by":[]}}},"body"]"#;
    let mut ignored = row("malformed keys", "malformed options");
    ignored.index_type = "btree".into();
    ignored.table_name = "not_selected".into();
    let catalog = catalog(&[
        ignored,
        row(
            keys,
            r#"{"analyzer":"later","ANALYZER":"\u2003first\u2003","ignored":"validated"}"#,
        ),
        row(r#"["body"]"#, r#"{"analyzer":"first"}"#),
    ]);
    let plain = tables();
    let expected = restore(&catalog, &plain).unwrap();
    let tables = Arc::new(tables());
    let before = tables.table.fields();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input =
        GinAnalyzerCatalogInput::new(&catalog, tables.clone(), LIMITS, &control).unwrap();
    assert!(input.advance(1).unwrap().is_none());
    let actual = finish(&mut input).unwrap();
    assert_eq!(
        tables.table.fields().as_slice(),
        plain.table.fields().as_slice()
    );
    assert!(before.is_empty());
    assert_eq!(*tables.visits.lock(), *plain.visits.lock());
    observe(actual, &expected, &control);
    assert!(input.advance(1).is_err());
    drop((input, tables, before));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn gin_input_preserves_complete_physical_before_semantic_and_per_field_error_order() {
    let mut missing = row("malformed", "malformed");
    missing.table_name = "missing".into();
    let mut large = row(r#"["body"]"#, "{}");
    large.definition_json = Some("x".repeat(4096));
    let catalog = catalog(&[missing.clone(), large]);
    let tables = Arc::new(tables());
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = GinAnalyzerCatalogInput::new(
        &catalog,
        tables.clone(),
        CatalogInputLimits {
            maximum_value_bytes: 1024,
            ..LIMITS
        },
        &control,
    )
    .unwrap();
    assert!(finish(&mut input).is_err());
    assert!(tables.visits.lock().is_empty());
    drop((input, tables));
    assert_eq!(control.memory().used(), 0);
    for (rows, fields, error) in [
        (
            vec![missing],
            vec![],
            "GIN analyzer owner references a missing table",
        ),
        (
            vec![row(r#"["body","count"]"#, "{}")],
            vec!["body", "count"],
            "set_table_analyzer: column `public.docs`.`count` must be TEXT, got Integer",
        ),
        (
            vec![
                row(r#"["body"]"#, r#"{"analyzer":"first"}"#),
                row(r#"["body"]"#, r#"{"analyzer":"second"}"#),
            ],
            vec!["body"],
            "GIN definitions have competing analyzer owners",
        ),
    ] {
        let catalog = self::catalog(&rows);
        let tables = Arc::new(super::tables());
        let mut input =
            GinAnalyzerCatalogInput::new(&catalog, tables.clone(), LIMITS, &control).unwrap();
        assert_eq!(finish(&mut input).err().unwrap().to_string(), error);
        assert_eq!(
            tables
                .table
                .fields()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            fields
        );
        assert!(input.advance(1).is_err());
        drop((input, tables));
        assert_eq!(control.memory().used(), 0);
    }
    for (keys, parameters) in [
        (r#"["body",{"Invalid":null}]"#, "{}"),
        (r#"["count"]"#, r#"{"analyzer":"first","invalid":false}"#),
    ] {
        let catalog = self::catalog(&[row(keys, parameters)]);
        let tables = Arc::new(super::tables());
        let mut input =
            GinAnalyzerCatalogInput::new(&catalog, tables.clone(), LIMITS, &control).unwrap();
        assert!(finish(&mut input).is_err());
        assert!(tables.table.fields().is_empty());
        drop((input, tables));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn gin_input_retires_partial_maps_and_field_generations_on_terminal_input() {
    let long = "field".repeat(32768);
    let keys = serde_json::to_string(&["body", long.as_str()]).unwrap();
    let catalog = catalog(&[row(&keys, r#"{"analyzer":"first"}"#)]);
    for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
        let mut tables = super::tables();
        tables.table.declared = false;
        let tables = Arc::new(tables);
        let control = StorageReadControl::with_limit(16 << 20);
        let mut input =
            GinAnalyzerCatalogInput::new(&catalog, tables.clone(), LIMITS, &control).unwrap();
        for _ in 0..100_000 {
            assert!(input.advance(1).unwrap().is_none());
            if !tables.table.fields().is_empty() {
                break;
            }
        }
        assert_eq!(tables.table.fields().as_slice(), ["body"]);
        let occupied = (terminal == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if terminal == "cancel" {
            control.cancellation().cancel();
        }
        if terminal == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandoned GIN restoration");
                }))
                .is_err()
            );
        } else {
            if terminal == "zero" {
                assert!(input.advance(0).is_err());
            } else if terminal != "drop" {
                assert!(finish(&mut input).is_err());
            }
            drop(input);
        }
        assert_eq!(tables.table.fields().as_slice(), ["body"]);
        drop((tables, occupied));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn gin_table_lookup_preserves_quoted_names_and_suspends_long_comparisons() {
    use uqa_core::memory::OwnedMap;
    let prefix = "한".repeat(16384);
    let map: OwnedMap<_, _> = [
        (RelationIdentity::new("public", "docs"), 1),
        (RelationIdentity::new("A.Schema", format!("{prefix}\"a")), 2),
        (RelationIdentity::new("A.Schema", format!("{prefix}\"b")), 3),
    ]
    .into_iter()
    .collect();
    let queries = map
        .iter()
        .map(|(name, value)| (name.qualified_name(), Some(*value)))
        .chain([("missing".into(), None), ("docs".into(), Some(1))])
        .collect::<Vec<_>>();
    let control = StorageReadControl::with_limit(8192);
    let mut input = ImageDecoder::<(), StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            for (query, expected) in queries {
                assert_eq!(
                    lookup_table_from(&map, &query, &mut input).await?.copied(),
                    expected
                );
            }
            Ok(())
        },
    )
    .unwrap();
    let mut polls = 0;
    loop {
        polls += 1;
        if input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
            .is_some()
        {
            break;
        }
    }
    assert!(polls > 32);
    drop(input);
    assert_eq!(control.memory().used(), 0);
}
