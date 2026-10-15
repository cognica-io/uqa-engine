//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected domain tuples preserve logical identity and stop at their consumer's boundary.

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    projection::CatalogRequest,
    test_support::{empty_catalog, CatalogServices},
};
use std::ops::ControlFlow;
use uqa_core::{catalog_identity::CatalogObjectIdentity, RelationIdentity};
use uqa_sql::catalog::{domain::StoredDomain, roles::RoleIdentity};

fn domain() -> StoredDomain {
    let uqa_sql::Statement::CreateDomain(mut definition) = uqa_sql::compile(
        "CREATE DOMAIN public.positive AS integer CONSTRAINT nn NOT NULL CONSTRAINT valid CHECK (true)",
    )
    .unwrap()
    .remove(0)
    else {
        panic!("expected a domain declaration");
    };
    definition.not_null.as_mut().unwrap().catalog_identity = Some(CatalogObjectIdentity {
        object_id: [1; 16],
        oid: 60_010,
    });
    definition.checks[0].catalog_identity = Some(CatalogObjectIdentity {
        object_id: [2; 16],
        oid: 60_011,
    });
    StoredDomain {
        object_id: [3; 16],
        oid: 60_000,
        array_oid: Some(60_001),
        identity: RelationIdentity::new("public", "positive"),
        owner: RoleIdentity::BOOTSTRAP,
        definition,
        array_name: None,
        usage_acl: None,
    }
}

#[test]
fn selected_domain_tuples_keep_identity_through_rename_and_encoding() {
    let catalog = empty_catalog();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let request = CatalogRequest::default();
    let mut domain = domain();
    let mut original = Vec::new();
    assert!(visit_domain_constraint_tuples(
        Some(&context),
        &catalog,
        &domain,
        &request,
        &mut |identity, tuple| {
            assert_eq!(tuple.expression().is_some(), identity.oid == 60_011);
            let decoded: ConstraintTuple =
                serde_json::from_str(&serde_json::to_string(&tuple).unwrap()).unwrap();
            let row = decoded.into_row();
            assert_eq!(row["oid"], Value::Int(identity.oid));
            assert_eq!(row["contypid"], Value::Int(60_000));
            assert_eq!(row["conrelid"], Value::Int(0));
            original.push((identity, serde_json::to_string(&tuple).unwrap()));
            Ok(ControlFlow::Continue(()))
        },
    )
    .unwrap()
    .is_continue());
    assert_eq!(original.len(), 2);

    // A domain name is not part of its constraint tuple; DDL retains the selected type OID.
    domain.identity.name = "renamed".into();
    domain.definition.name = "public.renamed".into();
    let mut renamed = Vec::new();
    let _ = visit_domain_constraint_tuples(
        Some(&context),
        &catalog,
        &domain,
        &request,
        &mut |identity, tuple| {
            renamed.push((identity, serde_json::to_string(&tuple).unwrap()));
            Ok(ControlFlow::Continue(()))
        },
    )
    .unwrap();
    assert_eq!(renamed, original);
}

#[test]
fn selected_domain_tuple_visits_stop_before_unused_rows() {
    let catalog = empty_catalog();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let request = CatalogRequest::default();
    let mut domain = domain();
    let selected = domain
        .definition
        .not_null
        .as_ref()
        .unwrap()
        .catalog_identity;
    // A later malformed constraint must remain unobserved after a successful consumer break.
    domain.definition.checks[0].catalog_identity = None;
    let mut visits = 0;
    assert!(visit_domain_constraint_tuples(
        Some(&context),
        &catalog,
        &domain,
        &request,
        &mut |identity, _| {
            visits += 1;
            assert_eq!(Some(identity), selected);
            Ok(ControlFlow::Break(()))
        },
    )
    .unwrap()
    .is_break());
    assert_eq!(visits, 1);

    let error =
        visit_domain_constraint_tuples(Some(&context), &catalog, &domain, &request, &mut |_, _| {
            Err(SQLError::Routine {
                sqlstate: "57014".into(),
                message: "selected tuple consumer cancelled".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(error.to_string(), "selected tuple consumer cancelled");
}
