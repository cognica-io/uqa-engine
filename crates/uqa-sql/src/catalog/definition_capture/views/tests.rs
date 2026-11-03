//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::security::BoundTableSecurity,
    plan::{RelationalPlan, SourcePlan, UnifiedPlan},
    ScalarExpr,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn view(sql: &str) -> StoredView {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    StoredView {
        security: BoundTableSecurity::owner(crate::catalog::roles::RoleIdentity {
            oid: 42,
            object_id: [42; 16],
        }),
        definition: StoredViewDefinition {
            object_id: [7; 16],
            query: *query,
            output_columns: Some(vec!["value".into()]),
            persistence: crate::ast::RelationPersistence::Permanent,
            options: vec![("security_barrier".into(), "true".into())],
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            materialized_column_collations: None,
            populated: true,
            catalog_oids: None,
            row_type_array_name: Some("_view_original".into()),
        },
    }
}

#[test]
fn view_capture_preserves_relational_syntax_materialized_bits_and_full_authority() {
    for sql in [
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "SELECT sum(a) OVER w FROM docs WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT a, count(*) FROM docs GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING count(*) > 0",
        "SELECT (SELECT 1), x.a FROM docs x LEFT JOIN other USING(a) WHERE EXISTS(SELECT 1) ORDER BY x.a LIMIT 2",
        "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
        "WITH changed AS (DELETE FROM docs RETURNING a) SELECT * FROM changed",
    ] {
        let mut original = view(sql);
        original.kind = StoredViewKind::Materialized;
        original.materialized_rows = vec![BTreeMap::from([("value".into(), Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)))]), BTreeMap::from([("value".into(), Value::Float(-0.0))]), BTreeMap::new()];
        original.materialized_column_types = vec![Some(crate::ColumnType::DoublePrecision)];
        original.materialized_column_collations = Some(vec![0]);
        original.security.acl = Some(Vec::new());
        original.security.acl_revisions.relation = Some([8;16]);
        original.security.acl_revisions.columns.insert("removed".into(), [9;16]);
        let memory=MemoryBudget::new(1<<20); let token=CancellationToken::new();
        let control=ProductionControl::new(&memory,&token,&token);
        let mut saved=None;
        let allocation=allocation_counter::measure(|| saved=Some(original.capture_definition(&control).unwrap()));
        let saved=saved.unwrap();
        assert_eq!(allocation.bytes_current, i64::try_from(memory.used()).unwrap());
        assert_eq!(serde_json::to_value(&saved.query).unwrap(), serde_json::to_value(&original.query).unwrap(), "{sql}");
        assert_eq!(saved.object_id, original.object_id);
        assert_eq!(saved.output_columns, original.output_columns);
        assert_eq!(saved.persistence, original.persistence);
        assert_eq!(saved.options, original.options);
        assert_eq!(saved.kind, original.kind);
        assert_eq!(saved.populated, original.populated);
        assert_eq!(saved.catalog_oids, original.catalog_oids);
        assert_eq!(saved.row_type_array_name, original.row_type_array_name);
        assert_eq!(saved.materialized_column_types, original.materialized_column_types);
        assert_eq!(saved.materialized_column_collations, original.materialized_column_collations);
        assert_eq!(saved.security.privileges.role_owner, original.security.role_owner);
        assert_eq!(saved.security.privileges.acl(), Some([].as_slice()));
        assert_eq!(saved.security.relation_revision, Some([8;16]));
        assert_eq!(saved.security.column_revisions, [("removed".into(), [9;16])]);
        for (row, original) in saved.materialized_rows.iter().zip(&original.materialized_rows) {
            assert_eq!(row.len(), original.len());
            for ((name, value),(old_name, old_value)) in row.iter().zip(original) { assert_eq!(name, old_name); assert!(value.has_same_representation(old_value)); }
        }
        drop((saved, original)); assert_eq!(memory.used(),0);
    }
}

#[test]
fn view_capture_detaches_constants_and_excludes_execution_only_query_owners() {
    for uncontrolled in [false, true] {
        let mut original = view("SELECT 'value' FROM docs");
        let source_memory = MemoryBudget::new(1 << 20);
        let source_token = CancellationToken::new();
        let source_control = ProductionControl::new(&source_memory, &source_token, &source_token);
        let value = crate::ir::RetainedConstantValue::from_produced(
            source_control
                .copy_value(&Value::Str("original".repeat(8192)))
                .unwrap(),
            &source_control,
        )
        .unwrap();
        let literal = crate::ir::RetainedScalarLiteral::with_control(
            value,
            None,
            None,
            Some(950),
            &source_control,
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = &mut original.query.root else {
            panic!("block")
        };
        block.projections[0].expr = ScalarExpr::RetainedLiteral(literal);
        let SourcePlan::Table {
            executable_query, ..
        } = block.from.as_mut().unwrap()
        else {
            panic!("table")
        };
        *executable_query = Some(Box::new(
            view(&format!("SELECT '{}'", "x".repeat(1 << 20)))
                .definition
                .query,
        ));
        original.query.analyzed_output = Some(crate::RowSchema::with_types(
            vec!["value".into()],
            vec![Some(crate::ColumnType::Text)],
        ));
        original.query.relation_privilege_subject = Some(
            crate::catalog::roles::RoleReference::Named("session role".into()),
        );
        source_token.cancel();
        let memory = MemoryBudget::new(256 * 1024);
        let token = CancellationToken::new();
        let control = if uncontrolled {
            ProductionControl::uncontrolled()
        } else {
            ProductionControl::new(&memory, &token, &token)
        };
        let saved = original.capture_definition(&control).unwrap();
        assert!(saved.query.analyzed_output.is_none());
        assert!(saved.query.relation_privilege_subject.is_none());
        let RelationalPlan::QueryBlock(block) = &saved.query.root else {
            panic!("block")
        };
        let SourcePlan::Table {
            executable_query, ..
        } = block.from.as_ref().unwrap()
        else {
            panic!("table")
        };
        assert!(executable_query.is_none());
        let ScalarExpr::RetainedLiteral(literal) = &block.projections[0].expr else {
            panic!("literal")
        };
        assert_eq!(literal.collation_oid, Some(950));
        drop(original);
        assert_eq!(source_memory.used(), 0);
        assert!(literal
            .value
            .resident_value()
            .unwrap()
            .has_same_representation(&Value::Str("original".repeat(8192))));
        drop(saved);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn view_capture_releases_prior_rows_and_catalog_entries_on_denial_or_cancellation() {
    let mut original = view("SELECT 1");
    original.materialized_rows = vec![BTreeMap::from([(
        "value".into(),
        Value::Str("x".repeat(65536)),
    )])];
    let source = BTreeMap::from([
        (RelationIdentity::new("s", "first"), view("SELECT 1")),
        (RelationIdentity::new("s", "last"), original),
    ]);
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 1 << 20 } else { 4096 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let error = views(&source, &ProductionControl::new(&memory, &token, &token)).unwrap_err();
        if cancelled {
            assert!(matches!(error, SQLError::Cancelled(_)));
        } else {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn view_capture_rejects_execution_scoped_rows_before_retaining_their_owner() {
    let mut source = view("SELECT 1 FROM docs");
    let relation = crate::ast::InternalRelationId::allocate();
    let schema = crate::RowSchema::with_internal_relation_types(
        relation,
        vec![Some(crate::ColumnType::Integer)],
    );
    let rows = crate::ast::RetainedRowSource::new(relation, schema, 1).unwrap();
    let RelationalPlan::QueryBlock(block) = &mut source.query.root else {
        panic!("block")
    };
    block.from = Some(SourcePlan::RetainedRows(rows));
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let error = source
        .capture_definition(&ProductionControl::new(&memory, &token, &token))
        .unwrap_err();
    assert!(
        matches!(error,SQLError::Internal(message) if message.contains("execution-scoped row source"))
    );
    assert_eq!(memory.used(), 0);
}
