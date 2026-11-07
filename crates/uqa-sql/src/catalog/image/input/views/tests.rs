//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::{
        roles::RoleIdentity,
        security::{table_binding::BoundTableAclEntry, TablePrivileges},
        stored_view::CapturedViewDefinitions,
    },
    plan::{RelationalPlan, UnifiedPlan},
};
use uqa_core::{memory::ProductionControl, RelationIdentity, Value};

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

fn capture(values: &ViewDefinitions, memory: &MemoryBudget) -> Budgeted<CapturedViewDefinitions> {
    let token = CancellationToken::new();
    values
        .capture(&ProductionControl::new(memory, &token, &token))
        .unwrap()
        .into_budgeted()
        .unwrap()
}

fn bytes(values: &CapturedViewDefinitions, width: usize) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut reader = values.image_reader(&memory, &token).unwrap();
    let mut output = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16_384);
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    drop(reader);
    assert_eq!(memory.used(), 0);
    output
}

fn fixture() -> (ViewDefinitions, Vec<u8>) {
    let mut values = ViewDefinitions::new();
    values.insert(name(0), view(0)).unwrap();
    let memory = MemoryBudget::new(16 << 20);
    let data = bytes(&capture(&values, &memory), 127);
    assert_eq!(memory.used(), 0);
    (values, data)
}

#[test]
fn native_view_images_preserve_authority_syntax_metadata_and_evaluated_bits() {
    let (mut original, data) = fixture();
    let mut legacy = view(1);
    legacy.persistence = RelationPersistence::Unlogged;
    legacy.kind = StoredViewKind::View;
    legacy.output_columns = None;
    legacy.catalog_oids = None;
    legacy.materialized_column_collations = None;
    legacy.row_type_array_name = None;
    legacy.populated = false;
    original.insert(name(1), legacy).unwrap();
    let mut temporary = view(2);
    temporary.persistence = RelationPersistence::Temporary;
    original.insert(name(2), temporary).unwrap();
    let capture_memory = MemoryBudget::new(16 << 20);
    let captured = capture(&original, &capture_memory);
    let data = bytes(&captured, data.len() * 2);
    assert_eq!(data, bytes(&captured, 1));
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let restored = ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
    assert_eq!(restored.len(), 2);
    for index in 0..2 {
        same(&original[&name(index)], &restored[&name(index)]);
    }
    assert_eq!(data, bytes(&capture(&restored, &capture_memory), 17));
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_view_images_share_immutable_roots_and_fund_independent_capture() {
    let (_, data) = fixture();
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut restored = ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
    let capture_memory = MemoryBudget::new(16 << 20);
    let mut saved = None;
    let production = ProductionControl::new(&capture_memory, &token, &token);
    let allocations = allocation_counter::measure(|| {
        saved = Some(
            restored
                .capture(&production)
                .unwrap()
                .into_budgeted()
                .unwrap(),
        );
    });
    assert_eq!(allocations.count_total, 0);
    let saved = saved.unwrap();
    let capture_bytes = capture_memory.used();
    assert!(capture_bytes >= memory.used());
    let denied = MemoryBudget::new(capture_bytes - 1);
    assert!(restored
        .capture(&ProductionControl::new(&denied, &token, &token))
        .is_err());
    assert_eq!(denied.used(), 0);
    let exact = MemoryBudget::new(capture_bytes);
    drop(capture(&restored, &exact));
    assert_eq!(exact.used(), 0);
    let clone = restored.clone();
    assert!(restored.shares_roots(&clone));
    restored
        .edit(&name(0), |view| {
            view.populated = false;
            Ok(())
        })
        .unwrap();
    assert!(!restored[&name(0)].populated);
    assert!(saved.get(&name(0)).unwrap().populated);
    drop(restored);
    drop(clone);
    assert!(memory.used() > 0);
    assert_eq!(bytes(&saved, 31), data);
    let allocations = allocation_counter::measure(|| drop(saved));
    assert_eq!(allocations.count_total, 0);
    assert_eq!(capture_memory.used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_view_images_reject_truncation_order_duplicates_and_trailing_data() {
    let (_, data) = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    for length in 0..data.len() {
        assert!(
            ViewDefinitions::read_image(&mut &data[..length], &memory, &token).is_err(),
            "length {length}"
        );
        assert_eq!(memory.used(), 0, "length {length}");
    }
    let mut trailing = data.clone();
    trailing.push(0);
    assert!(ViewDefinitions::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    let mut duplicate = data.clone();
    duplicate[8..16].copy_from_slice(&2u64.to_le_bytes());
    duplicate.extend_from_slice(&data[16..]);
    assert!(ViewDefinitions::read_image(&mut duplicate.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_view_images_admit_exact_peak_and_release_failed_publication() {
    let (_, data) = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    drop(ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap());
    let peak = memory.peak();
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, peak / 4, peak / 2, peak - 1] {
        let memory = MemoryBudget::new(limit);
        assert!(
            ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).is_err(),
            "limit {limit}"
        );
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(peak);
    let restored = ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
    let remaining = memory.reserve(peak - memory.used()).unwrap();
    let allocations = allocation_counter::measure(|| drop(restored));
    assert_eq!(allocations.count_total, 0);
    drop(remaining);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_view_images_capture_deep_query_retirement_without_stack_growth() {
    let (original, mut data) = fixture();
    let token = CancellationToken::new();
    let scratch = MemoryBudget::new(8 << 20);
    let mut query_reader = original[&name(0)]
        .query
        .image_reader(&scratch, &token)
        .unwrap();
    let mut query_bytes = 0;
    let mut buffer = [0; 127];
    loop {
        let count = query_reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        query_bytes += count;
    }
    data.truncate(data.len() - (query_bytes - 16));
    for level in 0..=10_000 {
        data.push(0);
        data.extend(0u64.to_le_bytes());
        data.push(2);
        data.extend(1u64.to_le_bytes());
        data.extend(1u64.to_le_bytes());
        if level == 10_000 {
            data.extend(std::iter::repeat_n(14, 10_000));
        }
        data.push(0);
        data.extend(u64::from(level < 10_000).to_le_bytes());
    }
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let restored =
                ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
            let capture_memory = MemoryBudget::new(128 << 20);
            let saved = capture(&restored, &capture_memory);
            drop(restored);
            let mut query = &saved.get(&name(0)).unwrap().query;
            let mut depth = 0;
            while let RelationalPlan::Values { subqueries, .. } = &query.root {
                if subqueries.is_empty() {
                    break;
                }
                query = &subqueries[0];
                depth += 1;
            }
            assert_eq!(depth, 10_000);
            let allocations = allocation_counter::measure(|| drop(saved));
            assert_eq!(allocations.count_total, 0);
            assert_eq!(memory.used(), 0);
            assert_eq!(capture_memory.used(), 0);
            for length in [data.len() - 1, data.len() - 100, data.len() / 2] {
                assert!(
                    ViewDefinitions::read_image(&mut &data[..length], &memory, &token).is_err()
                );
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

mod resources;
