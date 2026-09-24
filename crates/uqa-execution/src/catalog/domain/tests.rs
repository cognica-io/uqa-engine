//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RelationIdentity;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn encode(
    registry: &DomainRegistry,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<String> {
    uqa_sql::catalog::domain::validate_domain_definitions(registry, roles)
        .map_err(StorageBackendError::Other)?;
    Ok(serde_json::json!({"domain_catalog_format": 1, "domains": registry}).to_string())
}

fn legacy() -> BTreeMap<String, StoredDomain<String>> {
    let uqa_sql::Statement::CreateDomain(definition) =
        uqa_sql::compile("CREATE DOMAIN public.positive AS integer CHECK (VALUE > 0)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    BTreeMap::from([(
        "public.positive".into(),
        StoredDomain {
            object_id: [1; 16],
            oid: uqa_sql::catalog::domain::domain_object_oid(&[1; 16]),
            identity: RelationIdentity::new("public", "positive"),
            owner: "uqa".into(),
            definition,
        },
    )])
}

fn fixture() -> (KeyValueCatalog, BTreeMap<String, RoleDefinition>) {
    (
        KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
        BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
    )
}

fn restore(
    storage: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<DomainRegistry> {
    let loaded = super::restore(storage, roles, allow_migration)?;
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    snapshot.definitions.roles = Arc::new(roles.clone());
    snapshot.definitions.domains = Arc::new(loaded.registry);
    let catalog = crate::catalog::CatalogReadView::new(snapshot);
    let resolution = crate::catalog::RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_fixture".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: crate::catalog::RelationLookupMode::Bound,
    };
    Ok(
        super::finish_restore(storage, &catalog, &resolution, loaded.state)?
            .unwrap_or_else(|| (*catalog.snapshot().definitions.domains).clone()),
    )
}

#[test]
fn complete_legacy_conversion_preserves_domain_identity_and_never_rebinds_after_rename() {
    let (catalog, mut roles) = fixture();
    let legacy = legacy();
    catalog
        .set_metadata(
            DOMAINS_METADATA_KEY,
            &serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
    assert!(restore(&catalog, &roles, false)
        .unwrap_err()
        .to_string()
        .contains("initial catalog migration"));
    let converted = restore(&catalog, &roles, true).unwrap();
    let domain = &converted["public.positive"];
    assert_eq!(domain.object_id, legacy["public.positive"].object_id);
    assert_eq!(domain.oid, legacy["public.positive"].oid);
    assert_eq!(domain.owner, roles["uqa"].identity());
    let before = catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap();
    let mut owner = roles.remove("uqa").unwrap();
    owner.name = "renamed".into();
    roles.insert("renamed".into(), owner);
    let mut replacement = RoleDefinition::bootstrap();
    replacement.oid = 77;
    replacement.object_id = [9; 16];
    roles.insert("uqa".into(), replacement);
    assert_eq!(
        restore(&catalog, &roles, false).unwrap()["public.positive"].owner,
        domain.owner
    );
    assert_eq!(catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap(), before);
    roles.remove("renamed");
    assert!(restore(&catalog, &roles, true)
        .unwrap_err()
        .to_string()
        .contains("missing role incarnation"));
    assert_eq!(catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap(), before);
}

#[test]
fn invalid_legacy_candidates_are_rejected_before_any_conversion_write() {
    let (catalog, roles) = fixture();
    for invalid_owner in [true, false] {
        let mut registry = legacy();
        let mut invalid = registry["public.positive"].clone();
        invalid.identity.name = "z_invalid".into();
        invalid.definition.name = "public.z_invalid".into();
        if invalid_owner {
            invalid.owner = "missing".into();
        }
        registry.insert("public.z_invalid".into(), invalid);
        let json = serde_json::to_string(&registry).unwrap();
        catalog.set_metadata(DOMAINS_METADATA_KEY, &json).unwrap();
        assert!(restore(&catalog, &roles, true).is_err());
        assert_eq!(
            catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap().unwrap(),
            json
        );
    }
}

#[test]
fn malformed_aggregate_catalogs_never_fall_back_to_legacy_names() {
    let (catalog, roles) = fixture();
    let registry = legacy()
        .into_iter()
        .map(|(name, domain)| (name, domain.bind_owner(&roles).unwrap()))
        .collect();
    let current: serde_json::Value =
        serde_json::from_str(&encode(&registry, &roles).unwrap()).unwrap();
    for corruption in 0..5 {
        let mut value = current.clone();
        match corruption {
            0 => value["domain_catalog_format"] = 99.into(),
            1 => value["domains"]["public.positive"]["owner"] = "uqa".into(),
            2 => {
                value["domains"]["public.positive"]
                    .as_object_mut()
                    .unwrap()
                    .remove("owner");
            }
            3 => {
                value["domains"]["public.positive"]["owner"]["object_id"] =
                    serde_json::to_value([9_u8; 16]).unwrap();
            }
            _ => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("domain_catalog_format");
            }
        }
        let json = value.to_string();
        catalog.set_metadata(DOMAINS_METADATA_KEY, &json).unwrap();
        for allow_migration in [false, true] {
            assert!(restore(&catalog, &roles, allow_migration).is_err());
            assert_eq!(
                catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap().unwrap(),
                json
            );
        }
    }
}

mod records;

#[test]
fn empty_catalog_conversion_is_initial_only_and_idempotent() {
    let (catalog, roles) = fixture();
    assert!(restore(&catalog, &roles, false).is_err());
    assert!(catalog
        .get_metadata(DOMAINS_METADATA_KEY)
        .unwrap()
        .is_none());
    assert!(restore(&catalog, &roles, true).unwrap().is_empty());
    let before = catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap();
    assert!(restore(&catalog, &roles, false).unwrap().is_empty());
    assert_eq!(catalog.get_metadata(DOMAINS_METADATA_KEY).unwrap(), before);
}

#[test]
fn unchanged_domain_metadata_selects_the_committed_registry_and_validates_its_authority() {
    let (catalog, roles) = fixture();
    let committed = Arc::new(BTreeMap::from([(
        "public.positive".into(),
        records::domain("positive", 1),
    )]));
    let current = Arc::default();
    let selected = merge_private(Some(&catalog), &current, Arc::clone(&committed), &roles).unwrap();
    assert!(Arc::ptr_eq(&selected, &committed));
    assert!(current.is_empty());
    assert!(merge_private(Some(&catalog), &current, committed, &BTreeMap::new()).is_err());
    assert!(catalog
        .get_metadata(DOMAINS_METADATA_KEY)
        .unwrap()
        .is_none());
}
