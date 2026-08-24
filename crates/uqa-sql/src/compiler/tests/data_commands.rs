//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table/index alteration and data-command compilation.

use super::*;

#[test]
fn create_table_with_tensor_column() {
    let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(4))");
    let Statement::CreateTable(ct) = stmt else {
        panic!("not CREATE TABLE");
    };
    assert!(matches!(ct.columns[1].ty, ColumnType::Tensor(4)));
}

#[test]
fn create_index_records_access_method() {
    let stmt = first("CREATE INDEX idx_body ON docs USING gin (body)");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("not CREATE INDEX");
    };
    assert_eq!(ci.table, "docs");
    assert_eq!(ci.access_method, "gin");
    assert_eq!(ci.columns, vec!["body"]);
}

#[test]
fn table_commands_preserve_qualified_relation_names() {
    let stmt = first("ALTER TABLE app.docs ADD COLUMN version INTEGER");
    let Statement::AlterTable(alter) = stmt else {
        panic!("not ALTER TABLE");
    };
    assert_eq!(alter.table, "app.docs");

    let stmt = first("ALTER TABLE app.docs RENAME TO archived_docs");
    let Statement::AlterTable(rename) = stmt else {
        panic!("not ALTER TABLE RENAME");
    };
    assert_eq!(rename.table, "app.docs");

    let Statement::Update(update) = first("UPDATE app.docs SET version = 2") else {
        panic!("not UPDATE");
    };
    assert_eq!(update.table, "app.docs");

    let Statement::Delete(delete) = first("DELETE FROM app.docs") else {
        panic!("not DELETE");
    };
    assert_eq!(delete.table, "app.docs");

    let Statement::Truncate { tables, .. } = first("TRUNCATE app.docs") else {
        panic!("not TRUNCATE");
    };
    assert_eq!(tables, vec!["app.docs"]);

    let Statement::Insert(insert) = first("INSERT INTO app.docs (version) VALUES (1)") else {
        panic!("not INSERT");
    };
    assert_eq!(insert.table, "app.docs");
}

#[test]
fn alter_column_type_preserves_the_using_expression() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE metrics ALTER COLUMN value TYPE text USING (value + delta)::text")
    else {
        panic!("not ALTER TABLE ALTER COLUMN TYPE");
    };
    let crate::ast::AlterTableAction::AlterColumnType { name, ty, using } = alter.action else {
        panic!("not ALTER COLUMN TYPE");
    };
    assert_eq!(name, "value");
    assert_eq!(ty, ColumnType::Text);
    assert!(matches!(using, Some(Expr::Cast { ty, .. }) if ty == "text"));
}

#[test]
fn insert_with_array_literal() {
    let stmt = first(
        "INSERT INTO docs (id, title, embedding) VALUES \
         (1, 'rust language', ARRAY[0.1, 0.2, 0.3])",
    );
    let Statement::Insert(i) = stmt else {
        panic!("not INSERT");
    };
    assert_eq!(i.table, "docs");
    assert_eq!(i.columns, vec!["id", "title", "embedding"]);
    assert_eq!(i.rows.len(), 1);
    assert_eq!(i.rows[0].len(), 3);
    match &i.rows[0][2] {
        Expr::Array(v) => assert_eq!(v.len(), 3),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn insert_set_operation_is_compiled_as_one_select_source() {
    let Statement::Insert(insert) =
        first("INSERT INTO dst SELECT id FROM lhs UNION ALL SELECT id FROM rhs LIMIT 2 OFFSET 1")
    else {
        panic!("not INSERT");
    };
    assert!(insert.rows.is_empty());
    let source = insert
        .select_source
        .expect("INSERT must retain its SELECT source");
    let set_op = source
        .set_op
        .as_ref()
        .expect("INSERT source must retain its set operation");
    assert_eq!(set_op.kind, crate::ast::SetOpKind::Union);
    assert!(set_op.all);
    assert!(set_op.combined_limit.is_some());
    assert!(set_op.combined_offset.is_some());
}

#[test]
fn merge_not_matched_by_source_preserves_all_actions() {
    let Statement::Merge(merge) = first(
        "MERGE INTO target USING source ON target.id = source.id \
         WHEN NOT MATCHED BY SOURCE AND target.retired THEN UPDATE SET value = value + 1 \
         WHEN NOT MATCHED BY SOURCE AND target.expired THEN DELETE \
         WHEN NOT MATCHED BY SOURCE THEN DO NOTHING",
    ) else {
        panic!("expected MERGE");
    };

    assert!(matches!(
        merge.when_clauses.as_slice(),
        [
            crate::ast::MergeWhen::UpdateNotMatchedBySource { .. },
            crate::ast::MergeWhen::DeleteNotMatchedBySource { .. },
            crate::ast::MergeWhen::NothingNotMatchedBySource { .. }
        ]
    ));
}

#[test]
fn merge_rejects_a_clause_after_an_unconditional_clause_of_the_same_kind() {
    let error = compile(
        "MERGE INTO target USING source ON target.id = source.id \
         WHEN NOT MATCHED BY SOURCE THEN DO NOTHING \
         WHEN MATCHED THEN DO NOTHING \
         WHEN NOT MATCHED BY SOURCE AND target.retired THEN DELETE",
    )
    .unwrap_err();

    assert_eq!(error.sqlstate(), Some("42601"));
    assert!(error.to_string().contains("unreachable WHEN clause"));
}
