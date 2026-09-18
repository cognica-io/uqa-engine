//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{build_pg_authid, build_pg_roles};
use crate::catalog::{test_support::empty_catalog, CatalogReadView};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::Value;
use uqa_sql::{ast::RoleAttribute, catalog::roles::RoleDefinition};

#[test]
fn private_and_public_role_projections_share_one_snapshot_and_keep_password_masking_separate() {
    let mut role = RoleDefinition::bootstrap();
    role.oid = 20_000;
    role.object_id = [1; 16];
    role.name = "reader".into();
    role.connection_limit = 7;
    role.attributes = [RoleAttribute::Login, RoleAttribute::Replication].into();
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(role.name.clone(), role.clone())]));
    let original = CatalogReadView::new(snapshot.clone());
    role.name = "renamed".into();
    role.connection_limit = 9;
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(role.name.clone(), role)]));
    let updated = CatalogReadView::new(snapshot);

    for (catalog, expected_name, expected_limit) in
        [(&original, "reader", 7), (&updated, "renamed", 9)]
    {
        let private = build_pg_authid(catalog).remove(0);
        assert_eq!(private.len(), 12);
        assert_eq!(private["oid"], Value::Int(20_000));
        assert_eq!(private["rolname"], Value::Str(expected_name.into()));
        assert_eq!(private["rolconnlimit"], Value::Int(expected_limit));
        assert_eq!(private["rolpassword"], Value::Null);
        assert_eq!(private["rolvaliduntil"], Value::Null);
        for attribute in [
            "rolsuper",
            "rolinherit",
            "rolcreaterole",
            "rolcreatedb",
            "rolbypassrls",
        ] {
            assert_eq!(private[attribute], Value::Bool(false), "{attribute}");
        }
        for attribute in ["rolcanlogin", "rolreplication"] {
            assert_eq!(private[attribute], Value::Bool(true), "{attribute}");
        }
        let mut public = build_pg_roles(catalog).remove(0);
        assert_eq!(
            public.remove("rolpassword"),
            Some(Value::Str("********".into()))
        );
        assert_eq!(public.remove("rolconfig"), Some(Value::Null));
        public.insert("rolpassword".into(), Value::Null);
        assert_eq!(public, private);
    }
}
