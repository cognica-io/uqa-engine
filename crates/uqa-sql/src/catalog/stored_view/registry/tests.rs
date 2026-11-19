//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    catalog::{
        definition_capture::views::durable_views,
        roles::RoleIdentity,
        security::{table_binding::BoundTableAclEntry, BoundTableSecurity, TablePrivileges},
        stored_view::StoredViewDefinition,
        view::StoredViewKind,
    },
    plan::UnifiedPlan,
};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn name(index: u32) -> RelationIdentity {
    RelationIdentity::new("public", format!("view_{index:04}"))
}

fn view(index: u32) -> StoredView {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT 'original' AS body, -0.0 AS value")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query fixture")
    };
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let acl = BoundTableAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges {
            select: true,
            ..Default::default()
        },
    };
    security.acl = Some(vec![acl.clone()]);
    security.column_acls.insert("value".into(), vec![acl]);
    security.acl_revisions.relation = Some([7; 16]);
    security
        .acl_revisions
        .columns
        .insert("value".into(), [8; 16]);
    security
        .acl_revisions
        .columns
        .insert("removed".into(), [9; 16]);
    StoredView {
        security,
        definition: StoredViewDefinition {
            object_id: u128::from(index + 1).to_be_bytes(),
            query: *query,
            output_columns: Some(vec!["body".into(), "value".into()]),
            persistence: RelationPersistence::Permanent,
            options: vec![("security_barrier".into(), "true".into())],
            kind: StoredViewKind::Materialized,
            materialized_rows: vec![
                [
                    ("value".into(), Value::Float(-0.0)),
                    ("body".into(), Value::Str("original 한글".into())),
                ]
                .into_iter()
                .collect(),
                [(
                    "value".into(),
                    Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
                )]
                .into_iter()
                .collect(),
            ],
            materialized_column_types: vec![
                Some(crate::ColumnType::Text),
                Some(crate::ColumnType::DoublePrecision),
            ],
            materialized_column_collations: Some(vec![950, 0]),
            populated: true,
            catalog_oids: Some(crate::catalog::relation_oids::RelationCatalogOids {
                relation: 40000 + index * 4,
                row_type: Some(40001 + index * 4),
                array_type: Some(40002 + index * 4),
                rule: Some(40003 + index * 4),
            }),
            row_type_array_name: Some(format!("_view_{index:04}")),
        },
    }
}

fn same(left: &StoredView, right: &StoredView) {
    assert_eq!(
        serde_json::to_value(left).unwrap(),
        serde_json::to_value(right).unwrap()
    );
    assert_eq!(left.security, right.security);
    assert_eq!(left.materialized_rows.len(), right.materialized_rows.len());
    for (left, right) in left.materialized_rows.iter().zip(&right.materialized_rows) {
        assert_eq!(left.len(), right.len());
        for ((key, value), (old_key, old_value)) in left.iter().zip(right) {
            assert_eq!(key, old_key);
            assert!(value.has_same_representation(old_value));
        }
    }
}

#[test]
fn view_roots_capture_exact_syntax_authority_and_evaluated_bits_without_allocations() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(64 << 20);
    let control = ProductionControl::new(&memory, &token, &token);
    let original = view(0);
    let mut copied = None;
    let measured =
        allocation_counter::measure(|| copied = Some(original.copy_definition(&control).unwrap()));
    let bytes = memory.used();
    assert_eq!(measured.bytes_current, i64::try_from(bytes).unwrap());
    same(copied.as_ref().unwrap(), &original);
    drop(copied);
    assert_eq!(memory.used(), 0);
    let small = MemoryBudget::new(bytes - 1);
    assert!(original
        .copy_definition(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    for count in [1, 64, 4096] {
        let source = (0..count)
            .map(|index| (name(index), view(index)))
            .collect::<BTreeMap<_, _>>();
        let mut native = ViewDefinitions::try_from_iter(source.clone()).unwrap();
        let mut captured = None;
        let allocations =
            allocation_counter::measure(|| captured = Some(native.capture(&control).unwrap()));
        assert_eq!(allocations.count_total, 0);
        let bytes = memory.used();
        let exact = MemoryBudget::new(bytes);
        let independent = native
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), bytes);
        let legacy = durable_views(&source, &control).unwrap();
        native.clear();
        drop(native);
        for (((key, saved), (old_key, old)), (source_key, original)) in captured
            .as_ref()
            .unwrap()
            .iter()
            .zip(legacy.iter())
            .zip(&source)
        {
            assert_eq!(key, old_key);
            assert_eq!(key, source_key);
            same(saved, original);
            same(independent.get(key).unwrap(), original);
            assert_eq!(
                saved.security.role_owner,
                old.security.privileges.role_owner
            );
            assert_eq!(
                saved.security.acl_revisions.relation,
                old.security.relation_revision
            );
            assert!(saved.security.acl_revisions.columns.iter().eq(old
                .security
                .column_revisions
                .iter()
                .map(|(key, revision)| (key, revision))));
            assert_eq!(
                serde_json::to_value(&saved.query).unwrap(),
                serde_json::to_value(&old.query).unwrap()
            );
            for (row, previous) in saved.materialized_rows.iter().zip(&old.materialized_rows) {
                assert_eq!(row.len(), previous.len());
                for ((key, value), (previous_key, previous_value)) in row.iter().zip(previous) {
                    assert_eq!(key, previous_key);
                    assert!(value.has_same_representation(previous_value));
                }
                let old_map = row
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(
                    serde_json::to_vec(row).unwrap(),
                    serde_json::to_vec(&old_map).unwrap()
                );
            }
        }
        drop((captured, independent, legacy));
        assert_eq!(memory.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn selected_view_publication_shares_unrelated_large_queries_rows_and_authority() {
    for count in [1, 32, 512] {
        let mut native =
            ViewDefinitions::try_from_iter((0..count).map(|index| (name(index), view(index))))
                .unwrap();
        let mut large = view(9000);
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            crate::compile(&format!("SELECT '{}'", "q".repeat(1 << 20)))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        large.query = *query;
        large.materialized_rows[0].insert("body".into(), Value::Str("v".repeat(1 << 20)));
        large
            .security
            .column_acls
            .insert("column".repeat(1 << 17), vec![]);
        native.insert(name(9000), large).unwrap();
        let original = native.clone();
        let selected = name(0);
        let allocations = allocation_counter::measure(|| {
            native
                .edit(&selected, |view| {
                    view.options
                        .push(("security_invoker".into(), "true".into()));
                    view.security.acl_revisions.relation = Some([10; 16]);
                    Ok(())
                })
                .unwrap()
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        for key in original.keys().filter(|key| **key != selected) {
            assert!(native.shares_definition(key, &original));
        }
        same(&original[&selected], &view(0));
        assert!(native[&selected].security_invoker());
    }
}

#[test]
fn denied_view_publication_and_capture_retain_original_generations_and_release_leases() {
    let native_memory = MemoryBudget::new(65536);
    let mut native = ViewDefinitions::with_budget(&native_memory);
    let key = name(0);
    native.insert(key.clone(), view(0)).unwrap();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let captured = native.capture(&control).unwrap();
    let small = MemoryBudget::new(memory.used() - 1);
    assert!(native
        .capture(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    let blocked = native_memory.reserve(native_memory.available()).unwrap();
    assert!(native
        .edit(&key, |view| {
            view.populated = false;
            Ok(())
        })
        .is_err());
    same(&native[&key], &view(0));
    token.cancel();
    assert!(matches!(
        native.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((blocked, native));
    same(captured.get(&key).unwrap(), &view(0));
    assert!(native_memory.used() > 0);
    drop(captured);
    assert_eq!(native_memory.used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn temporary_view_roots_do_not_contribute_to_durable_capture_admission() {
    let mut native = ViewDefinitions::try_from_iter([(name(0), view(0))]).unwrap();
    let memory = MemoryBudget::new(32768);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let initial = native.capture(&control).unwrap();
    let bytes = memory.used();
    drop(initial);
    let mut temporary = view(1);
    temporary.persistence = RelationPersistence::Temporary;
    temporary.materialized_rows[0].insert("body".into(), Value::Str("x".repeat(1 << 20)));
    native.insert(name(1), temporary).unwrap();
    let captured = native.capture(&control).unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(memory.used(), bytes);
    native.remove(&name(0)).unwrap();
    let empty_memory = MemoryBudget::new(0);
    let allocations = allocation_counter::measure(|| {
        assert!(native
            .capture(&ProductionControl::new(&empty_memory, &token, &token))
            .unwrap()
            .is_empty());
    });
    assert_eq!(allocations.count_total, 0);
    drop((native, captured));
    assert_eq!(memory.used(), 0);
    assert_eq!(empty_memory.used(), 0);
}
