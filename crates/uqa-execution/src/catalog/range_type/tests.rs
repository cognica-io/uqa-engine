//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{catalog::range_type::StoredMultirange, ColumnType};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

mod projections;
mod snapshots;

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

fn definition() -> StoredRange {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/postgresql.json")).unwrap();
    let rows = reference["cases"][0]["results"][0]["rows"]
        .as_array()
        .unwrap();
    let oid = |name| {
        rows.iter().find(|row| row[1] == name).unwrap()[0]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    StoredRange {
        object_id: [1; 16],
        oid: oid("r"),
        array_oid: oid("_r"),
        array_name: "_r".into(),
        identity: RelationIdentity::new("public", "r"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1_978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [2; 16],
            oid: oid("r_multirange"),
            array_oid: oid("_r_multirange"),
            array_name: "_r_multirange".into(),
            identity: RelationIdentity::new("public", "r_multirange"),
        },
        usage_acl: None,
    }
}

struct Publication {
    catalog: KeyValueCatalog,
    registry: RefCell<RangeRegistry>,
}

impl Default for Publication {
    fn default() -> Self {
        Self {
            catalog: KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
            registry: RefCell::default(),
        }
    }
}

impl RangeRegistryPublication for Publication {
    fn range_registry(&self) -> RangeRegistryRead<'_> {
        Box::new(self.registry.borrow())
    }
    fn range_catalog(&self) -> Option<&dyn CatalogFacade> {
        Some(&self.catalog)
    }
    fn range_catalog_snapshot(&self) -> crate::catalog::CatalogReadView {
        crate::catalog::test_support::empty_catalog()
    }
    fn range_role_definitions(&self) -> BTreeMap<String, RoleDefinition> {
        roles()
    }
    fn publish_range_definitions(&self, registry: RangeRegistry) {
        *self.registry.borrow_mut() = registry;
    }
}

#[test]
fn range_records_require_all_four_exact_claims_and_a_known_format() {
    let registry = RangeRegistry::from([("public.r".into(), definition())]);
    for corruption in 0..9 {
        let publication = Publication::default();
        assert!(restore(&publication.catalog, &roles()).unwrap().is_empty());
        publish(&publication, &RangeRegistry::new(), registry.clone()).unwrap();
        let restored = restore(&publication.catalog, &roles()).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&registry).unwrap()
        );
        assert_eq!(
            publication
                .catalog
                .metadata_with_prefix(FORMAT.oid_prefix)
                .unwrap()
                .len(),
            4
        );
        let catalog = &publication.catalog;
        match corruption {
            0..=3 => catalog.delete_metadata(&format!(
                "{}{}",
                FORMAT.oid_prefix,
                definition().type_oids()[corruption]
            )),
            4 => catalog.set_metadata(&format!("{}40000", FORMAT.oid_prefix), "public.r"),
            5 => catalog.set_metadata(
                &format!("{}{}", FORMAT.oid_prefix, definition().oid),
                "public.other",
            ),
            6 => catalog.delete_metadata(FORMAT.format_key),
            7 => catalog.set_metadata(FORMAT.format_key, "{\"range_catalog_format\":2}"),
            _ => catalog.set_metadata(&FORMAT.key("public.r"), "{\"unrecognized\":1}"),
        }
        .unwrap();
        assert!(
            restore(catalog, &roles()).is_err(),
            "corruption {corruption}"
        );
    }
}

#[test]
fn range_publication_preserves_independent_changes_and_rejects_stale_replacement() {
    let publication = Publication::default();
    let original = RangeRegistry::from([("public.r".into(), definition())]);
    publish(&publication, &RangeRegistry::new(), original.clone()).unwrap();
    let mut other = definition();
    other.identity.schema = "another".into();
    other.multirange.identity.schema = "another".into();
    other.object_id = [3; 16];
    other.multirange.object_id = [4; 16];
    other.oid = 40_000;
    other.array_oid = 40_001;
    other.multirange.oid = 40_002;
    other.multirange.array_oid = 40_003;
    publish(
        &publication,
        &RangeRegistry::new(),
        RangeRegistry::from([("another.r".into(), other)]),
    )
    .unwrap();
    let mut renamed = original.clone();
    let multi = &mut renamed.get_mut("public.r").unwrap().multirange;
    multi.identity = RelationIdentity::new("moved", "renamed");
    multi.array_name = "_renamed".into();
    publish(&publication, &original, renamed.clone()).unwrap();
    let restored = restore(&publication.catalog, &roles()).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored["public.r"].multirange.identity, multi_identity());
    assert!(publish(&publication, &original, RangeRegistry::new())
        .unwrap_err()
        .to_string()
        .contains("changed during catalog publication"));
    publish(&publication, &renamed, RangeRegistry::new()).unwrap();
    assert_eq!(restore(&publication.catalog, &roles()).unwrap().len(), 1);
    assert_eq!(
        publication
            .catalog
            .metadata_with_prefix(FORMAT.oid_prefix)
            .unwrap()
            .len(),
        4
    );
}

fn multi_identity() -> RelationIdentity {
    RelationIdentity::new("moved", "renamed")
}

fn catalog(definition: StoredRange) -> crate::catalog::CatalogReadView {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    snapshot.definitions.ranges = Arc::new(RangeRegistry::from([(
        definition.identity.qualified_name(),
        definition,
    )]));
    snapshot.definitions.roles = Arc::new(roles());
    snapshot.definitions.schemas = Arc::new(BTreeMap::from([(
        "public".into(),
        crate::catalog::security::BoundSchemaSecurity::bootstrap("public"),
    )]));
    crate::catalog::CatalogReadView::new(snapshot)
}
