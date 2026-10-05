//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, CreateDomain};
use crate::catalog::domain::StoredDomain;
use crate::expr::composites::{CompositeAttribute, CompositeTypeDescriptor};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

#[derive(Default)]
struct Catalog {
    domains: BTreeMap<u32, StoredDomain>,
    composites: BTreeMap<u32, Arc<CompositeTypeDescriptor>>,
}

impl DomainCatalog for Catalog {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        self.domains.get(&oid).cloned()
    }
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(self.composites.get(&oid).cloned())
    }
}

fn domain(oid: u32, base: ColumnType) -> StoredDomain {
    let identity = RelationIdentity::new("public", format!("domain_{oid}"));
    StoredDomain {
        object_id: [7; 16],
        oid,
        array_oid: Some(oid + 1),
        definition: CreateDomain {
            name: identity.qualified_name(),
            base,
            collation: None,
            default: None,
            not_null: None,
            checks: Vec::new(),
        },
        identity,
        owner: RoleIdentity::BOOTSTRAP,
        array_name: None,
        usage_acl: None,
    }
}

fn composite(oid: u32) -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: format!("composite_{oid}"),
        oid,
        array_oid: oid + 1,
        relation_oid: oid - 1,
    })
}

fn descriptor_with(oid: u32, types: Vec<ColumnType>) -> Arc<CompositeTypeDescriptor> {
    Arc::new(CompositeTypeDescriptor {
        type_oid: oid,
        relation_oid: oid - 1,
        attributes: types
            .into_iter()
            .enumerate()
            .map(|(index, ty)| CompositeAttribute {
                name: format!("field_{index}"),
                ty,
                number: i16::try_from(index + 1).unwrap(),
            })
            .collect(),
    })
}

#[test]
fn direct_domain_inheritance_uses_live_definitions_instead_of_embedded_bases() {
    let target = domain(20_000, ColumnType::Integer);
    let child = domain(20_010, target.column_type());
    let stale_child = domain(child.oid, ColumnType::Text).column_type();
    let mut catalog = Catalog {
        domains: BTreeMap::from([(target.oid, target.clone()), (child.oid, child.clone())]),
        ..Catalog::default()
    };
    for ty in [target.column_type(), child.column_type(), stale_child] {
        assert_eq!(
            column_domain_dependency(&ty, target.oid, &catalog, &catalog).unwrap(),
            DomainColumnDependency::Direct,
        );
    }
    catalog
        .domains
        .insert(child.oid, domain(child.oid, ColumnType::Text));
    assert_eq!(
        column_domain_dependency(&child.column_type(), target.oid, &catalog, &catalog).unwrap(),
        DomainColumnDependency::None,
    );
}

#[test]
fn arrays_composites_and_domains_over_containers_require_container_validation() {
    let target = domain(20_000, ColumnType::Integer);
    let child = domain(20_010, target.column_type());
    let array_domain = domain(20_020, ColumnType::Array(Box::new(child.column_type())));
    let composite_domain = domain(20_030, composite(30_000));
    let catalog = Catalog {
        domains: [
            target.clone(),
            child.clone(),
            array_domain.clone(),
            composite_domain.clone(),
        ]
        .into_iter()
        .map(|domain| (domain.oid, domain))
        .collect(),
        composites: BTreeMap::from([
            (30_000, descriptor_with(30_000, vec![child.column_type()])),
            (30_010, descriptor_with(30_010, vec![composite(30_000)])),
        ]),
    };
    for ty in [
        ColumnType::Array(Box::new(target.column_type())),
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(child.column_type())))),
        array_domain.column_type(),
        composite_domain.column_type(),
        composite(30_010),
    ] {
        assert_eq!(
            column_domain_dependency(&ty, target.oid, &catalog, &catalog).unwrap(),
            DomainColumnDependency::Container,
            "{ty:?}",
        );
    }
    assert_eq!(
        column_domain_dependency(
            &array_domain.column_type(),
            array_domain.oid,
            &catalog,
            &catalog
        )
        .unwrap(),
        DomainColumnDependency::Direct,
    );
}

#[test]
fn unrelated_and_repeated_composite_paths_do_not_hide_a_domain_dependency() {
    let target = domain(20_000, ColumnType::Integer);
    let mut catalog = Catalog {
        domains: BTreeMap::from([(target.oid, target.clone())]),
        composites: BTreeMap::from([
            (
                30_000,
                descriptor_with(30_000, vec![composite(30_010), composite(30_010)]),
            ),
            (
                30_010,
                descriptor_with(30_010, vec![composite(30_000), ColumnType::Integer]),
            ),
        ]),
    };
    assert_eq!(
        column_domain_dependency(&composite(30_000), target.oid, &catalog, &catalog).unwrap(),
        DomainColumnDependency::None,
    );
    catalog.composites.insert(
        30_010,
        descriptor_with(30_010, vec![composite(30_000), target.column_type()]),
    );
    assert_eq!(
        column_domain_dependency(&composite(30_000), target.oid, &catalog, &catalog).unwrap(),
        DomainColumnDependency::Container,
    );
}

#[test]
fn missing_live_type_definitions_are_not_reported_as_unrelated_columns() {
    let catalog = Catalog::default();
    for ty in [
        domain(20_010, ColumnType::Integer).column_type(),
        composite(30_000),
    ] {
        assert!(column_domain_dependency(&ty, 20_000, &catalog, &catalog).is_err());
    }
}
