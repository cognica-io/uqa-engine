//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    test_support::{empty_catalog, table_snapshot, CatalogServices},
};
use std::sync::Arc;
use uqa_core::{catalog_identity::CatalogObjectIdentity, catalog_role::RoleIdentity};
use uqa_sql::{
    ast::TableConstraintSet,
    catalog::{domain::StoredDomain, roles::RoleDefinition},
    Statement,
};

fn fixture() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(
        [
            ("uqa".into(), RoleDefinition::bootstrap()),
            (reader.name.clone(), reader),
        ]
        .into(),
    );
    let Statement::CreateTable(mut table) = uqa_sql::compile("CREATE TABLE items(\"Has Space\" integer CONSTRAINT required NOT NULL, CONSTRAINT positive CHECK (\"Has Space\" > 0))").unwrap().remove(0) else { panic!("table"); };
    table.columns[0].not_null_identity = Some(CatalogObjectIdentity {
        object_id: [2; 16],
        oid: 30_000,
    });
    table.checks[0].object_id = Some([3; 16]);
    table.checks[0].catalog_oid = Some(30_001);
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot(
            [4; 16],
            table.columns,
            TableConstraintSet {
                checks: table.checks,
                ..TableConstraintSet::default()
            },
        ),
    );
    let Statement::CreateDomain(mut definition) = uqa_sql::compile("CREATE DOMAIN positive AS integer CONSTRAINT domain_required NOT NULL CONSTRAINT domain_positive CHECK(VALUE > 0)").unwrap().remove(0) else { panic!("domain"); };
    definition.not_null.as_mut().unwrap().catalog_identity = Some(CatalogObjectIdentity {
        object_id: [5; 16],
        oid: 30_002,
    });
    definition.checks[0].catalog_identity = Some(CatalogObjectIdentity {
        object_id: [6; 16],
        oid: 30_003,
    });
    Arc::make_mut(&mut snapshot.definitions.domains).insert(
        "public.positive".into(),
        StoredDomain {
            object_id: [7; 16],
            oid: 30_004,
            array_oid: Some(30_005),
            identity: RelationIdentity::new("public", "positive"),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
            array_name: None,
            usage_acl: None,
        },
    );
    CatalogReadView::new(snapshot)
}

fn checks(catalog: &CatalogReadView, subject: &str, request: &CatalogRequest) -> Vec<ResultRow> {
    let mut services = CatalogServices::default();
    services.resolution.current_user = subject.into();
    let output = RegtypeOutputCache::default();
    build_info_check_constraints(
        &services.context(catalog, &output),
        catalog,
        &services.resolution,
        request,
    )
    .unwrap()
}

#[test]
fn constraint_views_preserve_owner_visibility_and_decoded_not_null_names() {
    let catalog = fixture();
    let request = CatalogRequest::default();
    let rows = checks(&catalog, "uqa", &request);
    let clauses = rows
        .iter()
        .map(|row| (row["constraint_name"].clone(), row["check_clause"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        clauses,
        [
            (str_value("required"), str_value("Has Space IS NOT NULL")),
            (str_value("positive"), str_value("(\"Has Space\" > 0)")),
            (str_value("domain_required"), str_value("VALUE IS NOT NULL")),
            (str_value("domain_positive"), str_value("(VALUE > 0)")),
        ]
    );
    assert!(checks(&catalog, "reader", &request).is_empty());
}

#[test]
fn domain_constraint_visibility_uses_the_retained_type_usage_acl() {
    let original = fixture();
    let mut snapshot = original.snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.domains)
        .get_mut("public.positive")
        .unwrap()
        .usage_acl = Some(Vec::new());
    let restricted = CatalogReadView::new(snapshot);
    let mut resolution = CatalogServices::default().resolution;
    resolution.current_user = "reader".into();
    let request = CatalogRequest::default();
    assert_eq!(
        build_info_domain_constraints(&original, &resolution, &request)
            .unwrap()
            .len(),
        2
    );
    assert!(
        build_info_domain_constraints(&restricted, &resolution, &request)
            .unwrap()
            .is_empty()
    );
    resolution.current_user = "uqa".into();
    assert_eq!(
        build_info_domain_constraints(&restricted, &resolution, &request)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn requested_constraint_names_do_not_deparse_unselected_expressions() {
    let catalog = fixture();
    let mut snapshot = catalog.snapshot().clone();
    let domain = Arc::make_mut(&mut snapshot.definitions.domains)
        .get_mut("public.positive")
        .unwrap();
    let Statement::CreateDomain(definition) =
        uqa_sql::compile("CREATE DOMAIN positive AS integer CHECK(missing_routine(VALUE))")
            .unwrap()
            .remove(0)
    else {
        panic!("domain");
    };
    domain.definition.checks[0].expression = definition.checks[0].expression.clone();
    let catalog = CatalogReadView::new(snapshot);
    let mut request = CatalogRequest::columns(["constraint_name".into()]);
    request.require_name("constraint_name".into(), "domain_positive".into());
    assert_eq!(checks(&catalog, "uqa", &request).len(), 1);
    let mut request = CatalogRequest::default();
    request.require_name("constraint_name".into(), "required".into());
    assert_eq!(
        checks(&catalog, "uqa", &request)[0]["check_clause"],
        str_value("Has Space IS NOT NULL")
    );
}

#[test]
fn table_constraint_visibility_distinguishes_read_and_write_privileges() {
    let original = fixture();
    let mut resolution = CatalogServices::default().resolution;
    resolution.current_user = "reader".into();
    assert!(build_info_table_constraints(&original, &resolution)
        .unwrap()
        .is_empty());
    for (column, privileges) in [
        (None, TableAclPrivilege::ALL.as_slice()),
        (Some("Has Space"), TableAclPrivilege::COLUMN_ALL.as_slice()),
    ] {
        for privilege in privileges {
            let mut snapshot = original.snapshot().clone();
            let reader = &snapshot.definitions.roles["reader"];
            let entry = uqa_sql::catalog::security::table_binding::BoundTableAclEntry {
                role: Some(RoleIdentity {
                    oid: reader.oid,
                    object_id: reader.object_id,
                }),
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: privilege.mask(),
                grant_options: uqa_core::catalog_acl::TablePrivileges::default(),
            };
            let security = Arc::make_mut(
                &mut snapshot
                    .tables
                    .get_mut(&RelationIdentity::new("public", "items"))
                    .unwrap()
                    .security,
            );
            if let Some(column) = column {
                security.column_acls.insert(column.into(), vec![entry]);
            } else {
                security.acl = Some(vec![entry]);
            }
            let catalog = CatalogReadView::new(snapshot);
            let rows = build_info_table_constraints(&catalog, &resolution).unwrap();
            let read_only = matches!(
                privilege,
                TableAclPrivilege::Select | TableAclPrivilege::Maintain
            );
            assert_eq!(
                rows.len(),
                if read_only { 0 } else { 2 },
                "{column:?} {privilege:?}"
            );
            for row in rows {
                assert_eq!(row["table_catalog"], row["constraint_catalog"]);
                assert_eq!(row["table_catalog"], str_value("uqa"));
            }
        }
    }
}
