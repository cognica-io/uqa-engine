//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::roles::{RoleDefinition, RoleIdentity},
    schema::domains::constraints,
};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

fn sample(index: u32) -> StoredDomain {
    let identity = RelationIdentity::new("public", format!("domain_{index:04}"));
    let uqa_sql::Statement::CreateDomain(mut definition) = uqa_sql::compile(&format!(
        "CREATE DOMAIN {} AS integer DEFAULT 7 NOT NULL CHECK (VALUE > 0)",
        identity.qualified_name()
    ))
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    constraints::assign_names(&mut definition, &std::collections::BTreeSet::new()).unwrap();
    let mut next = u128::from(index + 1) << 64;
    constraints::materialize(&mut definition, &mut |_: &str| {
        next += 1;
        Ok(next.to_le_bytes())
    })
    .unwrap();
    let object_id = u128::from(index + 1).to_le_bytes();
    StoredDomain {
        oid: uqa_sql::catalog::domain::domain_object_oid(&object_id),
        object_id,
        array_oid: None,
        identity,
        owner: RoleIdentity::BOOTSTRAP,
        definition,
        array_name: None,
        usage_acl: None,
    }
}

fn fixture(count: u32) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let registry = DomainRegistry::try_from_iter((0..count).rev().map(|index| {
        let domain = sample(index);
        (domain.identity.qualified_name(), domain)
    }))
    .unwrap();
    super::super::records::migrate(catalog.as_ref(), &registry).unwrap();
    catalog
}

fn finish(
    input: &mut DomainCatalogInput,
    steps: usize,
) -> StorageBackendResult<(RestoredDomains, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("domain input did not progress")
}

#[test]
fn persistent_domain_input_preserves_native_roots_and_future_mutation_ownership() {
    for count in [0, 3, 64] {
        let catalog = fixture(count);
        let original = catalog.metadata_with_prefix("").unwrap();
        let expected = serde_json::to_value(
            super::super::restore(catalog.as_ref(), &roles(), false)
                .unwrap()
                .registry,
        )
        .unwrap();
        let mut visits = Vec::new();
        for steps in [1, 7, 64] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let captures = MemoryBudget::new(4 << 20);
            let mut input = DomainCatalogInput::new(
                Arc::clone(&catalog),
                roles(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            let (restored, polls) = finish(&mut input, steps).unwrap();
            visits.push(polls);
            let mut actual = restored.registry;
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
            let later = sample(100);
            let name = later.identity.qualified_name();
            actual.insert(name.clone(), later).unwrap();
            assert!(actual.contains_key(&name) && captured.get(&name).is_none());
            assert_eq!(captured.len(), usize::try_from(count).unwrap());
            drop((captured, actual, input));
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(
                (control.memory().used(), captures.used(), mutations.used()),
                (0, 0, 0)
            );
        }
        assert!(visits[0] > visits[1] && visits[1] >= visits[2]);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
    }
}

#[test]
fn persistent_domain_input_retires_every_partial_owner_without_catalog_writes() {
    let catalog = fixture(64);
    let original = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 16, 128, 512] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let mut input = DomainCatalogInput::new(
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
                        panic!("retire domain input");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                let error = finish(&mut input, usize::from(terminal != "zero"))
                    .err()
                    .expect("terminal input");
                if terminal == "cancel" {
                    assert!(
                        matches!(error, StorageBackendError::Cancelled(_))
                            || uqa_sql::catalog::errors::storage_error("domain", &error).sqlstate()
                                == Some("57014"),
                        "{error}"
                    );
                }
                if terminal == "memory" {
                    assert!(
                        matches!(error, StorageBackendError::Memory(_))
                            || uqa_sql::catalog::errors::storage_error("domain", &error).sqlstate()
                                == Some("53200"),
                        "{error}"
                    );
                }
                assert!(input.advance(1).is_err());
            }
            drop((input, occupied));
            assert_eq!(
                (control.memory().used(), mutations.used()),
                (0, 0),
                "{terminal}/{cut}"
            );
        }
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
}

fn failure(catalog: Arc<dyn CatalogFacade>, limits: CatalogInputLimits) -> StorageBackendError {
    let control = StorageReadControl::with_limit(16 << 20);
    let mut input =
        DomainCatalogInput::new(catalog, roles(), limits, control.memory(), &control).unwrap();
    let error = finish(&mut input, 1).err().expect("invalid domain catalog");
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    error
}

fn same_failure(catalog: Arc<dyn CatalogFacade>) {
    let expected = super::super::restore(catalog.as_ref(), &roles(), false)
        .err()
        .expect("ordinary failure");
    let original = catalog.metadata_with_prefix("").unwrap();
    let actual = failure(Arc::clone(&catalog), LIMITS);
    match expected {
        StorageBackendError::Serde(_) => {
            assert!(matches!(actual, StorageBackendError::Serde(_)), "{actual}");
        }
        expected => assert_eq!(actual.to_string(), expected.to_string()),
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
}

#[test]
fn persistent_domain_input_preserves_marker_claim_and_semantic_failure_order() {
    for corruption in [
        "missing marker",
        "legacy marker",
        "unknown marker field",
        "marker before syntax",
        "syntax before claim",
        "missing claim",
        "wrong claim",
        "leading zero",
        "overflow",
        "negative",
        "claim before identity",
        "owner",
        "constraint",
        "zero identity",
    ] {
        let catalog = fixture(2);
        let first = sample(0);
        let key = format!("{PREFIX}{}", first.identity.qualified_name());
        let claim = format!("{OID_PREFIX}{}", first.oid);
        let mut value = serde_json::to_value(&first).unwrap();
        match corruption {
            "missing marker" => catalog.delete_metadata(DOMAINS_METADATA_KEY).unwrap(),
            "legacy marker" => catalog
                .set_metadata(DOMAINS_METADATA_KEY, r#"{"domain_catalog_format":2}"#)
                .unwrap(),
            "unknown marker field" => catalog
                .set_metadata(
                    DOMAINS_METADATA_KEY,
                    r#"{"domain_catalog_format":3,"extra":null}"#,
                )
                .unwrap(),
            "marker before syntax" => {
                catalog.set_metadata(&key, "bad JSON").unwrap();
                catalog
                    .set_metadata(DOMAINS_METADATA_KEY, r#"{"domain_catalog_format":2}"#)
                    .unwrap();
            }
            "syntax before claim" => {
                catalog.set_metadata(&key, "bad JSON").unwrap();
                catalog.delete_metadata(&claim).unwrap();
            }
            "missing claim" => catalog.delete_metadata(&claim).unwrap(),
            "wrong claim" => catalog.set_metadata(&claim, "public.absent").unwrap(),
            "leading zero" | "overflow" | "negative" => {
                catalog.delete_metadata(&claim).unwrap();
                let suffix = match corruption {
                    "leading zero" => format!("0{}", first.oid),
                    "overflow" => "4294967296".into(),
                    _ => "-1".into(),
                };
                catalog
                    .set_metadata(
                        &format!("{OID_PREFIX}{suffix}"),
                        &first.identity.qualified_name(),
                    )
                    .unwrap();
            }
            _ => {
                match corruption {
                    "owner" => {
                        value["owner"]["object_id"] = serde_json::to_value([99_u8; 16]).unwrap();
                    }
                    "constraint" => {
                        value["definition"]["checks"][0]["catalog_identity"] =
                            serde_json::Value::Null;
                    }
                    _ => value["object_id"] = serde_json::to_value([0_u8; 16]).unwrap(),
                }
                catalog.set_metadata(&key, &value.to_string()).unwrap();
                if corruption == "claim before identity" {
                    catalog.delete_metadata(&claim).unwrap();
                }
            }
        }
        same_failure(catalog);
    }
}

#[test]
fn persistent_domain_input_keeps_marker_then_physical_then_native_field_precedence() {
    let catalog = fixture(2);
    let first = format!("{PREFIX}public.domain_0000");
    let last = format!("{PREFIX}public.domain_0001");
    catalog.set_metadata(&first, "bad definition JSON").unwrap();
    catalog.set_metadata(&last, &"x".repeat(4096)).unwrap();
    catalog
        .set_metadata(DOMAINS_METADATA_KEY, r#"{"domain_catalog_format":2}"#)
        .unwrap();
    let limits = CatalogInputLimits {
        maximum_value_bytes: 2048,
        ..LIMITS
    };
    assert!(failure(Arc::clone(&catalog), limits)
        .to_string()
        .contains("constraints require initial catalog migration"));
    catalog
        .set_metadata(DOMAINS_METADATA_KEY, r#"{"domain_catalog_format":3}"#)
        .unwrap();
    assert!(!matches!(
        failure(Arc::clone(&catalog), limits),
        StorageBackendError::Serde(_)
    ));
    catalog
        .set_metadata(&last, &serde_json::to_string(&sample(1)).unwrap())
        .unwrap();
    catalog
        .set_metadata(&format!("{OID_PREFIX}99999"), &"x".repeat(4096))
        .unwrap();
    assert!(matches!(
        failure(Arc::clone(&catalog), limits),
        StorageBackendError::Serde(_)
    ));
    catalog
        .set_metadata(&first, &serde_json::to_string(&sample(0)).unwrap())
        .unwrap();
    let error = failure(catalog, limits);
    assert!(
        !error.to_string().contains("OID records do not match"),
        "{error}"
    );
}
