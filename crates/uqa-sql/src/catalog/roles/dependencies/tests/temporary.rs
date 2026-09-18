//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Catalog, RelationIdentity, SequenceSecurity};
use crate::{
    ast::RelationPersistence,
    catalog::{
        roles::{dependencies::temporary::role_dependencies, RoleDefinition},
        security::{BoundTableSecurity, TableAclEntry, TablePrivileges, TableSecurity},
        stored_view::StoredView,
        view::StoredViewKind,
    },
    plan::UnifiedPlan,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::catalog_acl::AclGrantee;
use uqa_core::catalog_sequence::{SequenceAclEntry, SequencePrivileges};

fn roles() -> BTreeMap<String, RoleDefinition> {
    [
        "uqa",
        "owner",
        "reader",
        "grantor",
        "column_reader",
        "sequence_reader",
        "view_owner",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, name)| {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid += index as i64;
        if name != "uqa" {
            role.object_id = [index as u8 + 1; 16];
        }
        (name.into(), role)
    })
    .collect()
}

fn entry(role: impl Into<AclGrantee>, grantor: Option<&str>) -> TableAclEntry {
    TableAclEntry {
        role: role.into(),
        grantor: grantor.map(str::to_string),
        privileges: TablePrivileges {
            select: true,
            ..Default::default()
        },
        grant_options: TablePrivileges::default(),
    }
}

fn temporary_view() -> StoredView {
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query fixture")
    };
    StoredView {
        security: crate::catalog::security::BoundTableSecurity::bind(
            &crate::catalog::security::TableSecurity {
                role_owner: "view_owner".into(),
                acl: Some(vec![entry("reader", Some("uqa"))]),
                column_acls: BTreeMap::new(),
            },
            &roles(),
        )
        .unwrap(),
        definition: crate::catalog::stored_view::StoredViewDefinition {
            object_id: [7; 16],
            query: *query,
            output_columns: None,
            persistence: RelationPersistence::Temporary,
            options: Vec::new(),
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
        },
    }
}

#[test]
fn temporary_dependencies_include_owners_grantees_and_grantors_only_once() {
    let mut catalog = Catalog::new();
    catalog.table(
        "permanent",
        crate::catalog::roles::RoleIdentity {
            oid: 99,
            object_id: [99; 16],
        },
    );
    catalog.table("temporary", roles()["owner"].identity());
    let table = catalog
        .tables
        .get_mut(&RelationIdentity::new("public", "temporary"))
        .unwrap();
    table.persistence = RelationPersistence::Temporary;
    let mut named = TableSecurity::owner("owner");
    named.acl = Some(vec![
        entry("reader", Some("grantor")),
        entry(AclGrantee::Public, None),
    ]);
    named
        .column_acls
        .insert("v".into(), vec![entry("column_reader", Some("owner"))]);
    table.security = BoundTableSecurity::bind(&named, &roles()).unwrap();
    let sequence = RelationIdentity::new("pg_temp_1", "seq");
    catalog
        .sequence_persistence
        .insert(sequence.clone(), RelationPersistence::Temporary);
    catalog.sequences.insert(
        sequence,
        SequenceSecurity {
            role_owner: "uqa".into(),
            acl: Some(vec![SequenceAclEntry {
                role: "sequence_reader".into(),
                grantor: Some("grantor".into()),
                privileges: SequencePrivileges {
                    usage: true,
                    ..Default::default()
                },
                grant_options: SequencePrivileges::default(),
            }]),
        },
    );
    catalog.sequences.insert(
        RelationIdentity::new("public", "permanent"),
        SequenceSecurity {
            role_owner: "missing_permanent_owner".into(),
            acl: None,
        },
    );
    catalog
        .views
        .insert(RelationIdentity::new("pg_temp_1", "view"), temporary_view());
    assert_eq!(
        role_dependencies(&catalog, &roles(), 6).unwrap(),
        BTreeSet::from([11, 12, 13, 14, 15, 16])
    );
    assert_eq!(
        *catalog.events.borrow(),
        [
            "read tables",
            "security temporary",
            "release tables",
            "read views",
            "release views",
            "read sequences",
            "read sequence persistence",
            "release sequence persistence",
            "release sequences"
        ]
    );
    assert_eq!(
        role_dependencies(&catalog, &roles(), 5)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
}

#[test]
fn dangling_temporary_roles_fail_without_reading_later_catalogs() {
    let mut catalog = Catalog::new();
    catalog.table(
        "temporary",
        crate::catalog::roles::RoleIdentity {
            oid: 99,
            object_id: [99; 16],
        },
    );
    catalog.tables.values_mut().next().unwrap().persistence = RelationPersistence::Temporary;
    assert_eq!(
        role_dependencies(&catalog, &roles(), 10)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        *catalog.events.borrow(),
        ["read tables", "security temporary", "release tables"]
    );
}

#[test]
fn named_public_temporary_owners_grantees_and_grantors_retain_dependencies() {
    let mut roles = roles();
    let mut named = RoleDefinition::bootstrap();
    named.name = "PUBLIC".into();
    named.oid = 17;
    named.object_id = [17; 16];
    roles.insert(named.name.clone(), named);
    for (owner, acl, expected) in [
        ("PUBLIC", None, BTreeSet::from([17])),
        ("uqa", Some(entry("PUBLIC", None)), BTreeSet::from([17])),
        (
            "uqa",
            Some(entry("reader", Some("PUBLIC"))),
            BTreeSet::from([12, 17]),
        ),
    ] {
        let mut catalog = Catalog::new();
        catalog.table("temporary", roles[owner].identity());
        let table = catalog.tables.values_mut().next().unwrap();
        table.persistence = RelationPersistence::Temporary;
        let mut named = TableSecurity::owner(owner);
        named.acl = acl.map(|entry| vec![entry]);
        table.security = BoundTableSecurity::bind(&named, &roles).unwrap();
        assert_eq!(role_dependencies(&catalog, &roles, 10).unwrap(), expected);
    }
}
