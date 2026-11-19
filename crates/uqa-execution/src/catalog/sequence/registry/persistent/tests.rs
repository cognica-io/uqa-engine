//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::sequence::{catalog_oids, restoration};
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{roles::RoleDefinition, security::BoundSequenceSecurity},
};
use uqa_storage::{SequenceOptions, SequenceRow, SequenceSecurityRow};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 65536,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

fn row(index: u8) -> SequenceRow {
    SequenceRow {
        relation: RelationIdentity::new("public", format!("sequence_{index:03}_한글")),
        security: SequenceSecurityRow::Bound(uqa_core::catalog_sequence::BoundSequenceSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: Some(vec![uqa_core::catalog_role::BoundAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: uqa_core::catalog_sequence::SequencePrivileges {
                    select: true,
                    update: false,
                    usage: true,
                },
                grant_options: uqa_core::catalog_sequence::SequencePrivileges::default(),
            }]),
        }),
        object_id: [index + 1; 16],
        definition_generation: [index + 129; 16],
        start: 17,
        increment: 3,
        current: 20,
        called: true,
        log_count: 4,
        persistence: if index.is_multiple_of(2) { "p" } else { "u" }.into(),
        owner: Some(uqa_storage::SequenceOwner {
            table_object_id: [12; 16],
            column_object_id: [13; 16],
            dependency: uqa_storage::SequenceOwnerDependency::Internal,
        }),
        options: SequenceOptions {
            cache_size: 7,
            ..SequenceOptions::default()
        },
    }
}

fn fixture(count: u8) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    catalog
        .save_schema_row(&uqa_core::catalog_schema::BoundSchemaRow::bootstrap("public").into())
        .unwrap();
    for index in (0..count).rev() {
        let row = row(index);
        assert!(catalog.create_sequence_row(&row).unwrap());
        if index.is_multiple_of(2) {
            catalog_oids::record(catalog.as_ref(), &row.object_id, 40000 + u32::from(index))
                .unwrap();
        }
    }
    catalog
}

fn expected(catalog: &dyn CatalogFacade) -> StorageBackendResult<SequenceCatalog> {
    let mut values = restoration::prepare_sequence_rows(
        SequenceCatalog::default(),
        catalog.load_sequence_rows()?,
        &roles(),
    )?;
    values
        .replace_catalog_oids(catalog_oids::load(catalog)?)
        .map_err(sql_error)?;
    Ok(values)
}

fn finish(
    input: &mut PersistentSequenceCatalogInput,
    window: usize,
) -> StorageBackendResult<(SequenceCatalog, usize)> {
    for step in 1..65536 {
        if let Some(values) = input.advance(window)? {
            return Ok((values, step));
        }
    }
    panic!("sequence input stopped progressing")
}

#[test]
fn persistent_sequence_input_preserves_native_state_authority_oids_and_mutation_ownership() {
    for count in [0, 3, 64] {
        let catalog = fixture(count);
        // Recorded metadata can outlive its relation and remains part of the ordinary catalog root.
        catalog_oids::record(catalog.as_ref(), &[255; 16], 39999).unwrap();
        let expected = expected(catalog.as_ref()).unwrap();
        let original = catalog.load_sequence_rows().unwrap();
        let metadata = catalog.metadata_with_prefix("").unwrap();
        for window in [1, 7, 64] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let mut input = PersistentSequenceCatalogInput::new(
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            let (mut actual, steps) = finish(&mut input, window).unwrap();
            assert_eq!(
                actual.iter().collect::<Vec<_>>(),
                expected.iter().collect::<Vec<_>>()
            );
            assert_eq!(actual.security(), expected.security());
            assert_eq!(
                actual.catalog_oids().iter().collect::<Vec<_>>(),
                expected.catalog_oids().iter().collect::<Vec<_>>()
            );
            assert!(steps * window > usize::from(count));
            assert!(input.advance(1).is_err());
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            control.cancellation().cancel();
            let later = row(100);
            let (_, state) = restoration::sequence_state_from_row(later.clone()).unwrap();
            actual
                .insert(
                    later.relation,
                    SequenceRecord {
                        state,
                        object_id: later.object_id,
                        persistence: RelationPersistence::Permanent,
                        catalog_oid: Some(49999),
                    },
                    BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
                )
                .unwrap();
            assert!(
                actual.contains_durable_definition(&later.object_id, &later.definition_generation)
            );
            drop((actual, input));
            assert_eq!(mutations.used(), 0);
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
        assert_eq!(catalog.load_sequence_rows().unwrap(), original);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), metadata);
    }
}

#[test]
fn persistent_sequence_input_preserves_definition_authority_and_oid_error_order() {
    for defect in [
        "identity",
        "duplicate",
        "owner",
        "legacy",
        "increment",
        "log",
        "type",
        "generation",
        "bounds",
        "oid",
        "oid identity",
    ] {
        let catalog = fixture(0);
        let mut first = row(0);
        let mut second = row(1);
        match defect {
            "identity" => first.object_id = [0; 16],
            "duplicate" => second.object_id = first.object_id,
            "owner" => {
                if let SequenceSecurityRow::Bound(value) = &mut first.security {
                    value.role_owner.object_id = [0; 16];
                }
            }
            "legacy" => first.security = SequenceSecurityRow::legacy("uqa"),
            "increment" => first.increment = 0,
            "log" => first.log_count = -1,
            "type" => first.options.data_type = "unknown".into(),
            "generation" => first.definition_generation = [0; 16],
            "bounds" => first.options.min_value = Some(100),
            "oid" | "oid identity" => (),
            _ => unreachable!(),
        }
        catalog.create_sequence_row(&first).unwrap();
        catalog.create_sequence_row(&second).unwrap();
        // Every preceding row predicate must win over this later malformed OID record.
        catalog
            .set_metadata(
                "sequence_catalog_oid:invalid",
                if defect == "oid identity" {
                    "1"
                } else {
                    "invalid"
                },
            )
            .unwrap();
        let expected = expected(catalog.as_ref()).err().expect(defect).to_string();
        let original = catalog.load_sequence_rows().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = PersistentSequenceCatalogInput::new(
            Arc::clone(&catalog),
            roles(),
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        let actual = finish(&mut input, 1).err().expect(defect).to_string();
        assert_eq!(actual, expected, "{defect}");
        assert!(input.advance(1).is_err());
        assert_eq!(control.memory().used(), 0);
        assert_eq!(catalog.load_sequence_rows().unwrap(), original);
    }
}

#[test]
fn persistent_sequence_input_retires_every_pending_prefix_on_failure_drop_and_unwind() {
    let catalog = fixture(2);
    let mutations = MemoryBudget::new(1 << 20);
    let baseline = StorageReadControl::with_limit(1 << 20);
    let mut input = PersistentSequenceCatalogInput::new(
        Arc::clone(&catalog),
        roles(),
        LIMITS,
        &mutations,
        &baseline,
    )
    .unwrap();
    let (values, steps) = finish(&mut input, 1).unwrap();
    let peak = baseline.memory().peak();
    drop((values, input));
    assert_eq!(baseline.memory().used(), 0);
    for stop in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = PersistentSequenceCatalogInput::new(
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            for _ in 0..stop {
                assert!(input.advance(1).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    control.cancellation().cancel();
                    assert!(input.advance(1).is_err());
                }
                "zero" => assert!(input.advance(0).is_err()),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire sequence input");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                "drop" => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "{stop}: {terminal}");
        }
    }
    for limit in (0..peak).step_by(257).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        let result = PersistentSequenceCatalogInput::new(
            Arc::clone(&catalog),
            roles(),
            LIMITS,
            &mutations,
            &control,
        )
        .and_then(|mut input| finish(&mut input, 1));
        assert!(result.is_err(), "{limit}/{peak}");
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_sequence_oid_decimal_input_preserves_native_unsigned_parsing_and_last_key_wins() {
    let key = format!("sequence_catalog_oid:{}", "af".repeat(16));
    for value in [
        String::new(),
        "0".into(),
        "+0".into(),
        "-0".into(),
        "4294967295".into(),
        "4294967296".into(),
        " 1".into(),
        "١".into(),
        "++1".into(),
        format!("+{}17", "0".repeat(8192)),
    ] {
        let catalog = fixture(1);
        catalog
            .set_metadata(
                &key.to_uppercase()
                    .replacen("SEQUENCE_CATALOG_OID:", "sequence_catalog_oid:", 1),
                "9",
            )
            .unwrap();
        catalog.set_metadata(&key, &value).unwrap();
        let expected = expected(catalog.as_ref());
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = PersistentSequenceCatalogInput::new(
            catalog,
            roles(),
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        let actual = finish(&mut input, 1);
        match (expected, actual) {
            (Ok(expected), Ok((actual, steps))) => {
                assert_eq!(
                    actual.catalog_oids().iter().collect::<Vec<_>>(),
                    expected.catalog_oids().iter().collect::<Vec<_>>()
                );
                assert_eq!(
                    actual.catalog_oids().get(&[0xaf; 16]).copied(),
                    value.parse::<u32>().ok()
                );
                if value.len() > 8192 {
                    assert!(steps > value.len() / 256);
                }
            }
            (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
            _ => panic!("OID parsing differs for {value:?}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_sequence_assembly_adopts_original_name_and_acl_allocations() {
    let catalog = fixture(1);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = catalog.begin_sequence_load(LIMITS, &control).unwrap();
    let mut rows = loop {
        if let Some(rows) = source.advance(1).unwrap() {
            break rows;
        }
    };
    let row = rows.pop().unwrap().into_budgeted();
    let schema = row.relation.schema.as_ptr() as usize;
    let name = row.relation.name.as_ptr() as usize;
    let SequenceSecurityRow::Bound(authority) = &row.security else {
        unreachable!()
    };
    let acl = authority.acl.as_ref().unwrap().as_ptr() as usize;
    drop((rows, source));
    let mut input = ImageDecoder::<_, StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            let control =
                ProductionControl::new(input.memory, input.cancellation, input.cancellation);
            let mut result = SequenceCatalog::with_budget(input.memory);
            rows::insert(&mut result, row, &roles(), &control, &mut input).await?;
            Ok(result)
        },
    )
    .unwrap();
    let values = loop {
        if let Some(values) = input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break values;
        }
    };
    let relation = values.keys().next().unwrap();
    assert_eq!(relation.schema.as_ptr() as usize, schema);
    assert_eq!(relation.name.as_ptr() as usize, name);
    assert_eq!(
        values.security()[relation].acl.as_ref().unwrap().as_ptr() as usize,
        acl
    );
    drop((values, input));
    assert_eq!(control.memory().used(), 0);
}
