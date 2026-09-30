//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::catalog::enum_type::initial_enum_labels;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

fn definition(name: &str, base_oid: u32, labels: &[&str]) -> StoredEnum {
    let labels = labels
        .iter()
        .map(|label| (*label).to_owned())
        .collect::<Vec<_>>();
    let label_oids = (0..labels.len())
        .map(|index| base_oid + 2 + u32::try_from(index).unwrap())
        .collect::<Vec<_>>();
    StoredEnum {
        object_id: [u8::try_from(base_oid % 251).unwrap() + 1; 16],
        oid: base_oid,
        array_oid: base_oid + 1,
        array_name: format!("_{name}"),
        identity: RelationIdentity::new("public", name),
        owner: RoleIdentity::BOOTSTRAP,
        labels: initial_enum_labels(base_oid, &labels, &label_oids).unwrap(),
        usage_acl: None,
    }
}

struct Publication<'a> {
    catalog: &'a KeyValueCatalog,
    registry: RefCell<EnumRegistry>,
}

impl EnumRegistryPublication for Publication<'_> {
    fn enum_registry(&self) -> EnumRegistryRead<'_> {
        Box::new(self.registry.borrow())
    }
    fn enum_catalog(&self) -> Option<&dyn CatalogFacade> {
        Some(self.catalog)
    }
    fn enum_role_definitions(&self) -> BTreeMap<String, RoleDefinition> {
        roles()
    }
    fn publish_enum_definitions(&self, registry: EnumRegistry) {
        *self.registry.borrow_mut() = registry;
    }
}

#[test]
fn published_definitions_restore_with_every_oid_claim() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    assert!(restore(&catalog, &roles()).unwrap().is_empty());
    let publication = Publication {
        catalog: &catalog,
        registry: RefCell::new(EnumRegistry::new()),
    };
    let mood = definition("mood", 20_000, &["sad", "ok", "happy"]);
    let before = EnumRegistry::new();
    let mut after = before.clone();
    after.insert("public.mood".into(), mood.clone());
    publish(&publication, &before, after.clone()).unwrap();
    let restored = restore(&catalog, &roles()).unwrap();
    assert_eq!(
        serde_json::to_string(&restored).unwrap(),
        serde_json::to_string(&after).unwrap()
    );

    let mut renamed = after.clone();
    renamed
        .get_mut("public.mood")
        .unwrap()
        .rename_label("ok", "neutral")
        .unwrap();
    publish(&publication, &after, renamed.clone()).unwrap();
    assert_eq!(
        restore(&catalog, &roles()).unwrap()["public.mood"].labels[1].label,
        "neutral"
    );
    publish(&publication, &renamed, EnumRegistry::new()).unwrap();
    assert!(restore(&catalog, &roles()).unwrap().is_empty());
    assert!(catalog
        .metadata_with_prefix("uqa.sql.enum_oid.v1:")
        .unwrap()
        .is_empty());
}

#[test]
fn concurrent_changes_and_claimed_oids_are_rejected() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let publication = Publication {
        catalog: &catalog,
        registry: RefCell::new(EnumRegistry::new()),
    };
    let first = definition("mood", 20_000, &["a"]);
    let mut committed = EnumRegistry::new();
    committed.insert("public.mood".into(), first.clone());
    publish(&publication, &EnumRegistry::new(), committed.clone()).unwrap();

    // A statement that read the empty registry cannot replace the definition committed meanwhile.
    let mut stale = EnumRegistry::new();
    stale.insert("public.mood".into(), definition("mood", 30_000, &["b"]));
    let error = publish(&publication, &EnumRegistry::new(), stale).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed during catalog publication"),
        "{error}"
    );

    // Another type cannot claim an OID that the committed type owns.
    let mut colliding = committed.clone();
    let mut other = definition("other", 20_000, &["x"]);
    other.object_id = [9; 16];
    other.oid = 40_000;
    other.array_oid = 40_001;
    colliding.insert("public.other".into(), other);
    let error = publish(&publication, &committed, colliding).unwrap_err();
    assert!(error.to_string().contains("OID"), "{error}");
    assert_eq!(publication.registry.borrow().len(), 1);
}

#[test]
fn corrupt_records_fail_restoration() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let mood = definition("mood", 20_000, &["sad"]);
    catalog
        .set_metadata(
            "uqa.sql.enum.v1:public.mood",
            &serde_json::to_string(&mood).unwrap(),
        )
        .unwrap();
    assert!(restore(&catalog, &roles())
        .unwrap_err()
        .to_string()
        .contains("without their format marker"));
    catalog
        .set_metadata("sql_enums_json", r#"{"enum_catalog_format":1}"#)
        .unwrap();
    assert!(restore(&catalog, &roles())
        .unwrap_err()
        .to_string()
        .contains("OID records do not match"));
    catalog
        .set_metadata("sql_enums_json", r#"{"enum_catalog_format":2}"#)
        .unwrap();
    assert!(restore(&catalog, &roles())
        .unwrap_err()
        .to_string()
        .contains("unsupported enum catalog format 2"));
}
