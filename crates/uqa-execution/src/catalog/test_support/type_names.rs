//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog fixtures contain every routine user-type family and relation row-type owner.

use super::{CatalogReadSnapshot, CatalogReadView};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{
        composite_type::StoredComposite,
        domain::StoredDomain,
        enum_type::StoredEnum,
        range_type::{StoredMultirange, StoredRange},
        relation_oids::RelationCatalogOids,
        roles::{RoleDefinition, RoleDefinitions, RoleIdentity},
        security::BoundSchemaSecurity,
        stored_view::{StoredView, StoredViewDefinition},
    },
    ColumnType,
};

pub(crate) fn domain(schema: &str, name: &str) -> StoredDomain {
    let uqa_sql::Statement::CreateDomain(definition) =
        uqa_sql::compile("CREATE DOMAIN d AS integer")
            .unwrap()
            .remove(0)
    else {
        panic!("domain")
    };
    StoredDomain {
        object_id: [1; 16],
        oid: 41000,
        array_oid: Some(41001),
        identity: RelationIdentity::new(schema, name),
        owner: RoleIdentity::BOOTSTRAP,
        definition,
        array_name: None,
        usage_acl: None,
    }
}
pub(crate) fn catalog() -> CatalogReadView {
    let mut snapshot = super::empty_catalog().snapshot().clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 42;
    reader.object_id = [42; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = RetainedValue::new(
        RoleDefinitions::try_from_iter([
            ("uqa".into(), RoleDefinition::bootstrap()),
            ("reader".into(), reader),
        ])
        .unwrap(),
    );
    let mut schemas = BoundSchemaSecurity::initial_catalog();
    for name in ["other", "pg_temp_1", "private"] {
        schemas
            .insert(
                name.into(),
                if name == "private" {
                    BoundSchemaSecurity::bootstrap(name)
                } else {
                    BoundSchemaSecurity::with_public_privileges(true)
                },
            )
            .unwrap();
    }
    snapshot.definitions.schemas = RetainedValue::new(schemas);
    RetainedValue::make_mut(&mut snapshot.definitions.domains)
        .insert("public.d".into(), domain("public", "d"))
        .unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert(
            "public.e".into(),
            StoredEnum {
                object_id: [2; 16],
                oid: 42000,
                array_oid: 42001,
                array_name: "_e".into(),
                identity: RelationIdentity::new("public", "e"),
                owner: RoleIdentity::BOOTSTRAP,
                labels: Vec::new(),
                usage_acl: None,
            },
        )
        .unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.composites)
        .insert(
            "public.c".into(),
            StoredComposite {
                object_id: [3; 16],
                oid: 43000,
                relation_oid: 43002,
                array_oid: 43001,
                array_name: "_c".into(),
                identity: RelationIdentity::new("public", "c"),
                owner: RoleIdentity::BOOTSTRAP,
                attributes: Vec::new(),
                usage_acl: None,
            },
        )
        .unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.ranges)
        .insert(
            "public.r".into(),
            StoredRange {
                object_id: [4; 16],
                oid: 44000,
                array_oid: 44001,
                array_name: "_r".into(),
                identity: RelationIdentity::new("public", "r"),
                owner: RoleIdentity::BOOTSTRAP,
                subtype: ColumnType::Integer,
                subtype_operator_class: 1978,
                collation_oid: 0,
                canonical_oid: None,
                subtype_difference_oid: None,
                usage_acl: None,
                multirange: StoredMultirange {
                    object_id: [5; 16],
                    oid: 45000,
                    array_oid: 45001,
                    array_name: "_m".into(),
                    identity: RelationIdentity::new("public", "m"),
                },
            },
        )
        .unwrap();
    relations(&mut snapshot);
    CatalogReadView::new(snapshot)
}
fn oids(base: u32) -> RelationCatalogOids {
    let mut oids = RelationCatalogOids::legacy(
        uqa_sql::catalog::relation_oids::RelationOidKind::Table,
        &[6; 16],
    );
    oids.relation = base + 2;
    oids.row_type = Some(base);
    oids.array_type = Some(base + 1);
    oids
}
fn relations(snapshot: &mut CatalogReadSnapshot) {
    let mut table = super::table_snapshot(
        [6; 16],
        Vec::new(),
        uqa_sql::ast::TableConstraintSet::default(),
    );
    table.catalog_oids = oids(46000);
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "t"), table);
    let uqa_sql::plan::UnifiedPlan::Query(query) =
        uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS a").unwrap().remove(0))
    else {
        panic!("view query")
    };
    RetainedValue::make_mut(&mut snapshot.definitions.views)
        .insert(
            RelationIdentity::new("public", "v"),
            StoredView {
                security: uqa_sql::catalog::security::BoundTableSecurity::owner(
                    RoleIdentity::BOOTSTRAP,
                ),
                definition: StoredViewDefinition {
                    object_id: [7; 16],
                    catalog_oids: Some(oids(47000)),
                    row_type_array_name: Some("view_array".into()),
                    query: *query,
                    output_columns: None,
                    persistence: RelationPersistence::Permanent,
                    options: Vec::new(),
                    kind: uqa_sql::catalog::view::StoredViewKind::View,
                    materialized_rows: Vec::new(),
                    materialized_column_types: Vec::new(),
                    materialized_column_collations: None,
                    populated: true,
                },
            },
        )
        .unwrap();
    snapshot.definitions.foreign_tables = super::foreign_tables([(
        RelationIdentity::new("public", "f"),
        crate::catalog::foreign::StoredForeignTable {
            name: "f".into(),
            persistence: RelationPersistence::Permanent,
            object_id: [8; 16],
            catalog_oids: Some(oids(48000)),
            row_type_array_name: Some("foreign_array".into()),
            server_name: "fixture".into(),
            server_reference: None,
            columns: Vec::new(),
            dropped_attributes: Vec::new(),
            checks: Vec::new(),
            options: uqa_core::memory::OwnedMap::new(),
            option_order: Vec::new(),
        },
    )]);
}
