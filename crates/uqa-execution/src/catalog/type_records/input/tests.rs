//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::catalog::{
    enum_type::{initial_enum_labels, StoredEnum},
    roles::RoleDefinition,
    shell_type::StoredShellType,
};

mod ordering;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

trait Fixture: InputType {
    const FORMAT: TypeRecordFormat;
    fn sample(index: u32) -> (String, Self);
    fn restore(
        catalog: &dyn CatalogFacade,
        roles: &RoleDefinitions,
    ) -> StorageBackendResult<TypeDefinitions<Self>>;
}
impl Fixture for StoredEnum {
    const FORMAT: TypeRecordFormat = TypeRecordFormat {
        label: "enum",
        definition_context: "enum definition",
        oid_context: "enum OID",
        format_key: "sql_enums_json",
        format_field: "enum_catalog_format",
        version: 1,
        prefix: "uqa.sql.enum.v1:",
        oid_prefix: "uqa.sql.enum_oid.v1:",
    };
    fn sample(index: u32) -> (String, Self) {
        let name = format!("enum_{index:04}_한글");
        let oid = 20000 + index * 16;
        let identity = RelationIdentity::new("public", &name);
        (
            identity.qualified_name(),
            Self {
                object_id: [u8::try_from(index + 1).unwrap(); 16],
                oid,
                array_oid: oid + 1,
                array_name: format!("_{name}"),
                identity,
                owner: RoleIdentity::BOOTSTRAP,
                labels: initial_enum_labels(
                    oid,
                    &["a".into(), "한글".into(), "🙂".into()],
                    &[oid + 2, oid + 3, oid + 4],
                )
                .unwrap(),
                usage_acl: None,
            },
        )
    }
    fn restore(
        catalog: &dyn CatalogFacade,
        roles: &RoleDefinitions,
    ) -> StorageBackendResult<TypeDefinitions<Self>> {
        crate::catalog::enum_type::restore(catalog, roles)
    }
}
impl Fixture for StoredShellType {
    const FORMAT: TypeRecordFormat = TypeRecordFormat {
        label: "shell",
        definition_context: "shell definition",
        oid_context: "shell OID",
        format_key: "sql_shell_types_json",
        format_field: "shell_catalog_format",
        version: 1,
        prefix: "uqa.sql.shell.v1:",
        oid_prefix: "uqa.sql.shell_oid.v1:",
    };
    fn sample(index: u32) -> (String, Self) {
        let name = format!("shell_{index:04}_한글");
        let identity = RelationIdentity::new("public", &name);
        (
            identity.qualified_name(),
            Self {
                object_id: [u8::try_from(index + 1).unwrap(); 16],
                oid: 20000 + index * 16,
                identity,
                owner: RoleIdentity::BOOTSTRAP,
                usage_acl: None,
            },
        )
    }
    fn restore(
        catalog: &dyn CatalogFacade,
        roles: &RoleDefinitions,
    ) -> StorageBackendResult<TypeDefinitions<Self>> {
        crate::catalog::shell_type::restore(catalog, roles)
    }
}

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}
fn fixture<T: Fixture>(count: u32) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let definitions = TypeDefinitions::<T>::try_from_iter((0..count).rev().map(T::sample)).unwrap();
    T::FORMAT
        .persist(catalog.as_ref(), &TypeDefinitions::new(), &definitions)
        .unwrap();
    catalog.set_metadata("unrelated", "invalid JSON").unwrap();
    catalog
}
fn finish<T>(
    input: &mut TypeCatalogInput<T>,
    steps: usize,
) -> StorageBackendResult<(TypeDefinitions<T>, usize)> {
    for polls in 1..1_000_000 {
        if let Some(registry) = input.advance(steps)? {
            return Ok((registry, polls));
        }
    }
    panic!("type catalog input did not progress")
}

fn native_roots<T: Fixture>() {
    for count in [0, 3, 64] {
        let catalog = fixture::<T>(count);
        let original = catalog.metadata_with_prefix("").unwrap();
        let expected =
            serde_json::to_value(T::restore(catalog.as_ref(), &roles()).unwrap()).unwrap();
        let mut visits = Vec::new();
        for steps in [1, 7, 64] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let captures = MemoryBudget::new(4 << 20);
            let mut input = new::<T>(
                &T::FORMAT,
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            let (mut actual, polls) = finish(&mut input, steps).unwrap();
            visits.push(polls);
            assert_eq!(serde_json::to_value(&actual).unwrap(), expected);
            assert!(input.advance(1).is_err());
            let captured = actual
                .capture(&ProductionControl::new(
                    &captures,
                    control.cancellation(),
                    control.cancellation(),
                ))
                .unwrap();
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            control.cancellation().cancel();
            let (name, definition) = T::sample(100);
            actual.insert(name.clone(), definition).unwrap();
            assert!(actual.contains_key(&name) && captured.get(&name).is_none());
            assert_eq!(captured.len(), usize::try_from(count).unwrap());
            drop((captured, actual, input));
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(captures.used(), 0);
            assert_eq!(mutations.used(), 0);
        }
        assert!(visits[0] > visits[1] && visits[1] >= visits[2]);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
    }
}

#[test]
fn persistent_enum_input_preserves_native_roots_claims_and_future_mutation_ownership() {
    native_roots::<StoredEnum>();
}
#[test]
fn persistent_shell_input_preserves_native_roots_claims_and_future_mutation_ownership() {
    native_roots::<StoredShellType>();
}

fn terminal_owners<T: Fixture>() {
    let catalog = fixture::<T>(64);
    let original = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 16, 128, 512] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let mut input = new::<T>(
                &T::FORMAT,
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            for _ in 0..cut {
                assert!(input.advance(1).unwrap().is_none());
            }
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            match terminal {
                "cancel" => control.cancellation().cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire type catalog");
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

#[test]
fn persistent_enum_input_retires_physical_definition_and_claim_prefixes() {
    terminal_owners::<StoredEnum>();
}
#[test]
fn persistent_shell_input_retires_physical_definition_and_claim_prefixes() {
    terminal_owners::<StoredShellType>();
}
