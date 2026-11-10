//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_identity::CatalogObjectIdentity, catalog_index::CatalogIndexRow};
use uqa_sql::{
    ast::{ForeignKey, RelationPersistence},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
    ColumnType,
};

fn identity(id: u8) -> CatalogObjectIdentity {
    CatalogObjectIdentity {
        object_id: [id; 16],
        oid: 20_000 + i64::from(id),
    }
}

fn constraint_catalog(ordinary: u8, foreign: u8, domain: u8) -> CatalogReadSnapshot {
    let mut snapshot = snapshot();
    let column = |id| {
        let mut column = ColumnDef::nullable("id", ColumnType::Integer);
        column.not_null = true;
        column.not_null_identity = Some(identity(id));
        column
    };
    snapshot.tables.insert(
        RelationIdentity::new("public", "ordinary"),
        table_snapshot(
            [1; 16],
            vec![column(ordinary)],
            TableConstraintSet::default(),
        ),
    );
    snapshot.definitions.foreign_tables = crate::catalog::test_support::foreign_tables([(
        RelationIdentity::new("public", "remote"),
        crate::catalog::foreign::StoredForeignTable {
            name: "public.remote".into(),
            persistence: RelationPersistence::Permanent,
            object_id: [2; 16],
            catalog_oids: None,
            row_type_array_name: None,
            server_name: "remote_server".into(),
            server_reference: None,
            columns: vec![column(foreign)],
            dropped_attributes: vec![],
            checks: vec![],
            options: uqa_core::memory::OwnedMap::default(),
            option_order: vec![],
        },
    )]);
    RetainedValue::make_mut(&mut snapshot.definitions.domains)
        .insert(
            "public.positive".into(),
            uqa_sql::catalog::domain::StoredDomain {
                object_id: [3; 16],
                oid: 30_000,
                array_oid: Some(30_001),
                identity: RelationIdentity::new("public", "positive"),
                owner: uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
                definition: uqa_sql::ast::CreateDomain {
                    name: "public.positive".into(),
                    base: ColumnType::Integer,
                    collation: None,
                    collation_oid: None,
                    default: None,
                    not_null: Some(uqa_sql::ast::DomainNotNull {
                        name: Some("required".into()),
                        catalog_identity: Some(identity(domain)),
                    }),
                    checks: vec![],
                },
                array_name: Some("_positive".into()),
                usage_acl: None,
            },
        )
        .unwrap();
    snapshot
}

#[test]
fn native_catalog_constraints_share_one_namespace_across_tables_foreign_tables_and_domains() {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let valid = constraint_catalog(10, 11, 12);
    validate_constraints(&valid, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
    for (ordinary, foreign, domain) in [(10, 10, 12), (10, 11, 10), (10, 11, 11)] {
        let invalid = constraint_catalog(ordinary, foreign, domain);
        assert!(validate_constraints(&invalid, &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_catalog_constraint_validation_joins_original_foreign_key_targets_without_mutation() {
    let mut snapshot = snapshot();
    let parent = RelationIdentity::new("public", "parent");
    let child = RelationIdentity::new("public", "child");
    let column = || ColumnDef::nullable("id", ColumnType::Integer);
    let mut key: ForeignKey = serde_json::from_str(
        r#"{"local_columns":["id"],"ref_table":"public.parent","ref_columns":["id"]}"#,
    )
    .unwrap();
    key.referenced_index = Some([3; 16]);
    snapshot.tables.insert(
        parent.clone(),
        table_snapshot([1; 16], vec![column()], TableConstraintSet::default()),
    );
    snapshot.tables.insert(
        child.clone(),
        table_snapshot(
            [2; 16],
            vec![column()],
            TableConstraintSet {
                foreign_keys: vec![key],
                ..TableConstraintSet::default()
            },
        ),
    );
    RetainedValue::make_mut(&mut snapshot.definitions.catalog_indexes)
        .insert(
            CatalogIndexRow {
                relation: RelationIdentity::new("public", "saved"),
                table_name: parent.qualified_name(),
                index_type: "btree".into(),
                columns_json: r#"["id"]"#.into(),
                parameters_json: "{}".into(),
                definition_json: Some(
                    serde_json::to_string(&IndexDefinition {
                        unique: true,
                        catalog: Some(IndexCatalogIdentity {
                            identity: CatalogObjectIdentity {
                                object_id: [3; 16],
                                oid: 20003,
                            },
                            table_object_id: [1; 16],
                            physical_key: "original".into(),
                        }),
                        ..IndexDefinition::default()
                    })
                    .unwrap(),
                ),
            },
            RelationPersistence::Permanent,
        )
        .unwrap();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    validate_constraints(&snapshot, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
    let original = snapshot.tables.remove(&parent).unwrap();
    assert!(validate_constraints(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    snapshot.tables.insert(parent.clone(), original);
    snapshot.tables.get_mut(&parent).unwrap().object_id = [8; 16];
    assert!(validate_constraints(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    assert_eq!(
        snapshot.tables.get(&child).unwrap().foreign_keys[0].referenced_index,
        Some([3; 16])
    );
}

#[test]
fn native_catalog_constraint_triggers_share_the_constraint_identity_namespace() {
    for collision in ["none", "oid", "incarnation"] {
        let mut catalog = constraint_catalog(10, 11, 12);
        let trigger = uqa_sql::catalog::events::StoredTrigger {
            definition: uqa_sql::ast::CreateTrigger {
                name: "guard".into(),
                table: "public.ordinary".into(),
                function: "public.check_row".into(),
                arguments: vec![],
                constraint: true,
                referenced_table: None,
                deferrability: uqa_sql::ast::TriggerDeferrability::NotDeferrable,
                row: true,
                timing: uqa_sql::ast::TriggerTiming::After,
                events: vec![uqa_sql::ast::TriggerEvent::Insert],
                update_columns: vec![],
                transition_relations: vec![],
                when: None,
                or_replace: false,
            },
            function_object_id: None,
            enabled: uqa_sql::ast::EventEnableMode::Origin,
            object_id: Some(identity(if collision == "incarnation" { 10 } else { 13 }).object_id),
            constraint_name: Some("guard".into()),
            // The trigger row occupies pg_trigger, so its OID may equal a pg_constraint OID.
            catalog_oid: Some(identity(10).oid),
            constraint_catalog_oid: Some(identity(if collision == "oid" { 10 } else { 13 }).oid),
        };
        RetainedValue::make_mut(&mut catalog.definitions.triggers)
            .insert_event(
                RelationIdentity::new("public", "ordinary"),
                "guard".into(),
                trigger,
            )
            .unwrap();
        let memory = MemoryBudget::new(64 << 10);
        let result = validate_constraints(&catalog, &memory, &CancellationToken::new());
        assert_eq!(
            result.is_ok(),
            collision == "none",
            "{collision}: {result:?}"
        );
        assert_eq!(memory.used(), 0);
    }
}
