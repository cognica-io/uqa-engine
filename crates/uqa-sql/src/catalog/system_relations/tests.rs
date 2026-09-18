//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;

// PostgreSQL 18.4 rewrite range tables, traversed in LockViewRecurse order.
#[test]
fn system_view_sources_preserve_postgresql_reference_order() {
    for (name, expected) in [
        ("pg_catalog.pg_roles", "pg_catalog.pg_authid,pg_catalog.pg_db_role_setting"),
        ("pg_catalog.pg_shadow", "pg_catalog.pg_authid,pg_catalog.pg_db_role_setting"),
        ("pg_catalog.pg_user", "pg_catalog.pg_shadow"),
        ("pg_catalog.pg_rules", "pg_catalog.pg_rewrite,pg_catalog.pg_class,pg_catalog.pg_namespace"),
        ("pg_catalog.pg_views", "pg_catalog.pg_class,pg_catalog.pg_namespace"),
        ("pg_catalog.pg_tables", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_tablespace"),
        ("pg_catalog.pg_matviews", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_tablespace"),
        ("pg_catalog.pg_indexes", "pg_catalog.pg_index,pg_catalog.pg_class,pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_tablespace"),
        ("pg_catalog.pg_sequences", "pg_catalog.pg_sequence,pg_catalog.pg_class,pg_catalog.pg_namespace"),
        ("pg_catalog.pg_prepared_statements", ""),
        ("pg_catalog.pg_settings", ""),
        ("information_schema.information_schema_catalog_name", ""),
        ("information_schema.column_privileges", "pg_catalog.pg_namespace,pg_catalog.pg_authid,pg_catalog.pg_attribute,pg_catalog.pg_class,pg_catalog.pg_class,pg_catalog.pg_attribute,pg_catalog.pg_class,pg_catalog.pg_authid"),
        ("information_schema.columns", "pg_catalog.pg_attribute,pg_catalog.pg_attrdef,pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_type,pg_catalog.pg_namespace,pg_catalog.pg_type,pg_catalog.pg_namespace,pg_catalog.pg_collation,pg_catalog.pg_namespace,pg_catalog.pg_depend,pg_catalog.pg_sequence"),
        ("information_schema.enabled_roles", "pg_catalog.pg_authid"),
        ("information_schema.key_column_usage", "pg_catalog.pg_attribute,pg_catalog.pg_namespace,pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_constraint"),
        ("information_schema.role_column_grants", "information_schema.column_privileges,information_schema.enabled_roles,information_schema.enabled_roles"),
        ("information_schema.routines", "pg_catalog.pg_namespace,pg_catalog.pg_proc,pg_catalog.pg_language,pg_catalog.pg_type,pg_catalog.pg_namespace"),
        ("information_schema.schemata", "pg_catalog.pg_namespace,pg_catalog.pg_authid"),
        ("information_schema.sequences", "pg_catalog.pg_namespace,pg_catalog.pg_class,pg_catalog.pg_sequence,pg_catalog.pg_depend"),
        ("information_schema.table_constraints", "pg_catalog.pg_namespace,pg_catalog.pg_namespace,pg_catalog.pg_constraint,pg_catalog.pg_class,pg_catalog.pg_index"),
        ("information_schema.tables", "pg_catalog.pg_namespace,pg_catalog.pg_class,pg_catalog.pg_type,pg_catalog.pg_namespace"),
        ("information_schema.views", "pg_catalog.pg_namespace,pg_catalog.pg_class,pg_catalog.pg_trigger,pg_catalog.pg_trigger,pg_catalog.pg_trigger"),
    ] {
        let relation = SystemRelation::from_qualified_name(name).unwrap();
        let actual = relation.view_sources().iter().map(|source| source.qualified_name()).collect::<Vec<_>>().join(",");
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn system_identities_are_unique_and_reference_graph_is_closed_and_acyclic() {
    fn visit(relation: SystemRelation, ancestors: &mut Vec<SystemRelation>) {
        assert!(!ancestors.contains(&relation), "cycle at {relation:?}");
        ancestors.push(relation);
        for source in relation.view_sources() {
            assert_eq!(
                SystemRelation::from_qualified_name(&source.qualified_name()),
                Some(*source)
            );
            visit(*source, ancestors);
        }
        ancestors.pop();
    }
    let mut oids = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for relation in SystemRelation::all() {
        assert!(oids.insert(relation.oid()));
        assert!(ids.insert(relation.object_id()));
        assert_ne!(relation.object_id(), [0; 16]);
        if relation.kind() == "table" {
            assert!(relation.view_sources().is_empty());
        }
        visit(relation, &mut Vec::new());
    }
    assert_eq!(SystemRelation::at("pg_catalog", "PG_AUTHID"), None);
    assert_eq!(SystemRelation::from_qualified_name("pg_authid"), None);
    assert_eq!(SystemRelation::PgAuthid.oid(), 1260);
    assert_eq!(SystemRelation::PgSequence.oid(), 2224);
}

#[test]
fn system_security_preserves_public_settings_updates_and_private_role_catalogs() {
    use crate::catalog::{
        roles::RoleDefinition,
        security::table::{role_has_table_privilege, TableAclPrivilege},
    };
    use std::collections::BTreeMap;
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.attributes.clear();
    let roles = BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader),
    ]);
    for relation in SystemRelation::all() {
        let security = relation.bootstrap_security();
        for privilege in TableAclPrivilege::ALL {
            assert!(role_has_table_privilege(
                &security,
                "uqa",
                privilege,
                &roles,
                &BTreeMap::new()
            ));
            let expected = (privilege == TableAclPrivilege::Select
                && !matches!(
                    relation,
                    SystemRelation::PgAuthid
                        | SystemRelation::PgShadow
                        | SystemRelation::Projected(
                            VirtualRelation::AgGraph | VirtualRelation::AgLabel
                        )
                ))
                || (privilege == TableAclPrivilege::Update
                    && relation == SystemRelation::Projected(VirtualRelation::PgSettings));
            assert_eq!(
                role_has_table_privilege(&security, "reader", privilege, &roles, &BTreeMap::new()),
                expected,
                "{relation:?} {privilege:?}"
            );
        }
    }
}
