//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::restore;
use super::*;
use uqa_core::catalog_schema::SchemaTupleIdentity;
use uqa_sql::catalog::roles::RoleDefinition;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 65536,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

fn fixture(count: usize) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    catalog.set_metadata(BUILTIN_INITIALIZED, "true").unwrap();
    for index in 0..count {
        let name = format!("namespace_{index:04}_한글");
        let mut security = BoundSchemaSecurity::with_public_privileges(index % 2 == 0);
        security.tuple = Some(SchemaTupleIdentity::initial(
            40000 + u32::try_from(index).unwrap(),
        ));
        catalog.save_schema_row(&security.row(name).into()).unwrap();
    }
    catalog
}

fn finish(input: &mut SchemaCatalogInput) -> StorageBackendResult<(SchemaRegistry, usize)> {
    for step in 1..65536 {
        if let Some(values) = input.advance(1)? {
            return Ok((values, step));
        }
    }
    panic!("schema input stopped progressing");
}

#[test]
fn persistent_schema_input_matches_original_restoration_and_bounds_native_work() {
    for count in [0, 8, 128] {
        let catalog = fixture(count);
        let roles = roles();
        let expected = restore(&*catalog, &roles, false).unwrap();
        let stored = catalog.load_schema_rows().unwrap();
        let metadata = catalog.metadata_with_prefix("").unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let mut input =
            SchemaCatalogInput::new(Arc::clone(&catalog), roles, LIMITS, &mutations, &control)
                .unwrap();
        let (mut values, steps) = finish(&mut input).unwrap();
        assert_eq!(values, expected);
        assert!(steps > count);
        assert!(steps < (count + 1) * 128 + 64, "{count}: {steps}");
        assert_eq!(catalog.load_schema_rows().unwrap(), stored);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), metadata);
        assert!(input.advance(1).is_err());
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        values
            .insert("later".into(), BoundSchemaSecurity::bootstrap("later"))
            .unwrap();
        assert!(values.contains_key("later"));
        drop((values, input));
        assert_eq!(mutations.used(), 0);
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn persistent_schema_input_preserves_name_authority_identity_and_migration_error_order() {
    for defect in [
        "name",
        "legacy",
        "owner",
        "grantee",
        "grantor",
        "identity",
        "duplicate_oid",
        "duplicate_object",
        "builtin",
    ] {
        let catalog = fixture(0);
        let mut first = BoundSchemaSecurity::bootstrap("first");
        let mut second = BoundSchemaSecurity::with_public_privileges(false);
        second.tuple = Some(SchemaTupleIdentity::initial(40000));
        let missing = uqa_core::catalog_role::RoleIdentity {
            oid: 999,
            object_id: [9; 16],
        };
        match defect {
            "name" => catalog.save_schema_row(&SchemaRow::legacy("")).unwrap(),
            "legacy" => catalog.save_schema_row(&SchemaRow::legacy("a")).unwrap(),
            "owner" => second.role_owner = missing,
            "grantee" => {
                let entry = &mut second.acl.as_mut().unwrap()[0];
                entry.role = Some(missing);
                entry.grantor.oid = 0;
            }
            "grantor" => second.acl.as_mut().unwrap()[0].grantor = missing,
            "identity" => second.tuple = None,
            "duplicate_oid" => second.tuple.as_mut().unwrap().oid = first.tuple.unwrap().oid,
            "duplicate_object" => {
                second.tuple.as_mut().unwrap().object_id = first.tuple.unwrap().object_id;
            }
            "builtin" => (),
            _ => unreachable!(),
        }
        if defect == "name" {
            first.role_owner = missing;
        }
        catalog.save_schema_row(&first.row("first").into()).unwrap();
        catalog
            .save_schema_row(&second.row("second").into())
            .unwrap();
        catalog.delete_metadata(BUILTIN_INITIALIZED).unwrap();
        let roles = roles();
        let expected = restore(&*catalog, &roles, false).unwrap_err().to_string();
        let category = match defect {
            "name" => "invalid schema name",
            "legacy" => "schema security requires",
            "owner" | "grantee" | "grantor" => "missing role incarnation 999",
            "identity" => "schema identity requires",
            "duplicate_oid" | "duplicate_object" => "duplicate schema catalog identity",
            "builtin" => "built-in schemas require",
            _ => unreachable!(),
        };
        assert!(expected.contains(category), "{defect}: {expected}");
        let before = catalog.load_schema_rows().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = SchemaCatalogInput::new(
            Arc::clone(&catalog),
            roles,
            LIMITS,
            &MemoryBudget::new(1 << 20),
            &control,
        )
        .unwrap();
        assert_eq!(finish(&mut input).unwrap_err().to_string(), expected);
        assert!(input.advance(1).is_err());
        assert_eq!(control.memory().used(), 0);
        assert_eq!(catalog.load_schema_rows().unwrap(), before);
        assert_eq!(catalog.get_metadata(BUILTIN_INITIALIZED).unwrap(), None);
    }
}

#[test]
fn persistent_schema_input_retires_every_unpublished_prefix_on_cancel_zero_drop_and_unwind() {
    let catalog = fixture(3);
    let control = StorageReadControl::with_limit(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let mut input =
        SchemaCatalogInput::new(Arc::clone(&catalog), roles(), LIMITS, &mutations, &control)
            .unwrap();
    let (values, steps) = finish(&mut input).unwrap();
    drop((input, values));
    assert_eq!(control.memory().used(), 0);
    for prefix in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = SchemaCatalogInput::new(
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            for _ in 0..prefix {
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
                        panic!("abandon schema restoration");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                "drop" => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "{terminal} after {prefix}");
        }
    }
}

#[test]
fn persistent_schema_input_retires_each_denied_allocation_before_publication() {
    let catalog = fixture(3);
    let control = StorageReadControl::with_limit(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let mut input =
        SchemaCatalogInput::new(Arc::clone(&catalog), roles(), LIMITS, &mutations, &control)
            .unwrap();
    let (values, _) = finish(&mut input).unwrap();
    let peak = control.memory().peak();
    drop((input, values));
    for limit in (0..peak).step_by(127).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        let result =
            SchemaCatalogInput::new(Arc::clone(&catalog), roles(), LIMITS, &mutations, &control)
                .and_then(|mut input| finish(&mut input));
        assert!(result.is_err(), "limit {limit}, original peak {peak}");
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
}
