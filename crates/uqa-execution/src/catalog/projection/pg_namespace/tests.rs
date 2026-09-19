//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{test_support::empty_catalog, RelationLookupMode};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{ArrayValue, Value};
use uqa_sql::catalog::{roles::RoleDefinition, security::BoundSchemaSecurity};

#[test]
fn namespace_oid_projection_reads_the_captured_incarnation_after_name_reuse() {
    let mut snapshot = empty_catalog().snapshot().clone();
    let mut first = BoundSchemaSecurity::bootstrap("s");
    first.tuple = Some(uqa_core::catalog_schema::SchemaTupleIdentity {
        oid: 40_001,
        object_id: [1; 16],
        revision: [2; 16],
    });
    snapshot.definitions.schemas = Arc::new(BTreeMap::from([("s".into(), first.clone())]));
    let original = CatalogReadView::new(snapshot.clone());
    first.tuple = Some(uqa_core::catalog_schema::SchemaTupleIdentity {
        oid: 40_002,
        object_id: [3; 16],
        revision: [4; 16],
    });
    snapshot.definitions.schemas = Arc::new(BTreeMap::from([("s".into(), first)]));
    let replacement = CatalogReadView::new(snapshot);
    assert_eq!(super::super::schema_object_oid(&original, "s"), 40_001);
    assert_eq!(super::super::schema_object_oid(&replacement, "s"), 40_002);
    assert_eq!(
        super::super::schema_object_oid(&replacement, "pg_catalog"),
        11
    );
}

#[test]
fn namespace_snapshots_keep_owner_oids_and_project_acl_and_information_schema_names() {
    let owner = RoleDefinition::bootstrap();
    let mut roles = BTreeMap::from([(owner.name.clone(), owner)]);
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = Arc::new(BTreeMap::from([(
        "public".into(),
        BoundSchemaSecurity::bootstrap("public"),
    )]));
    snapshot.definitions.roles = Arc::new(roles.clone());
    let original = CatalogReadView::new(snapshot.clone());
    let mut owner = roles.remove("uqa").unwrap();
    let mut replacement = owner.clone();
    owner.name = "renamed owner".into();
    replacement.oid = 20_000;
    replacement.object_id = [9; 16];
    roles.insert(owner.name.clone(), owner);
    roles.insert(replacement.name.clone(), replacement);
    snapshot.definitions.roles = Arc::new(roles);
    let renamed = CatalogReadView::new(snapshot);
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    assert_eq!(
        original.schema_security("public"),
        renamed.schema_security("public")
    );
    for (catalog, owner, acl) in [
        (&original, "uqa", ["uqa=UC/uqa", "=U/uqa"]),
        (
            &renamed,
            "renamed owner",
            [
                "\"renamed owner\"=UC/\"renamed owner\"",
                "=U/\"renamed owner\"",
            ],
        ),
    ] {
        let row = build_pg_namespace(catalog, &resolution)
            .unwrap()
            .into_iter()
            .find(|row| row["nspname"] == Value::Str("public".into()))
            .unwrap();
        assert_eq!(row["nspowner"], Value::Int(10));
        assert_eq!(
            row["nspacl"],
            Value::Array(
                ArrayValue::try_new(acl.map(|value| Value::Str(value.into())).to_vec()).unwrap()
            )
        );
        let row = super::super::information_schema::build_info_schemata(catalog, &resolution)
            .unwrap()
            .into_iter()
            .find(|row| row["schema_name"] == Value::Str("public".into()))
            .unwrap();
        assert_eq!(row["schema_owner"], Value::Str(owner.into()));
    }
}
