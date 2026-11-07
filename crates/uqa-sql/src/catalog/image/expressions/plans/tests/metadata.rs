//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{CommandPlan, CtePlanBody, RelationalPlan, SourcePlan};

#[test]
fn query_images_exclude_execution_projections_without_visiting_them() {
    let mut value = query("SELECT * FROM t, (SELECT 1) AS q");
    let expected = encode(&value);
    value.analyzed_output = Some(crate::RowSchema::default());
    value.analyzed_source = Some(
        std::sync::Arc::new(crate::plan::AnalyzedSource {
            schema: crate::RowSchema::default(),
            children: None,
        })
        .into(),
    );
    value.relation_privilege_subject = Some("runtime-owner".into());
    let RelationalPlan::QueryBlock(block) = &mut value.root else {
        panic!("block")
    };
    let Some(SourcePlan::Join { left, right, .. }) = &mut block.from else {
        panic!("join")
    };
    let SourcePlan::Table {
        executable_query, ..
    } = left.as_mut()
    else {
        panic!("table")
    };
    let mut invalid = query("WITH q AS (SELECT 1) SELECT * FROM q");
    invalid.ctes[0].body = CtePlanBody::Command(Box::new(CommandPlan::Empty));
    *executable_query = Some(Box::new(invalid));
    let SourcePlan::Subquery {
        captured_output, ..
    } = right.as_mut()
    else {
        panic!("subquery")
    };
    *captured_output = Some(crate::ast::InternalRelationId::from_raw(987));
    assert_eq!(encode(&value), expected);
    value.relations_bound = true;
    assert_ne!(encode(&value), expected);
}

#[test]
fn query_image_authorization_columns_use_native_order_and_original_names() {
    let mut value = query("SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut value.root else {
        panic!("block")
    };
    block.projections.clear();
    for index in (0..1024).rev() {
        block.privilege_columns.insert(ColumnIdentity::qualified(
            "q.with.dot",
            format!("c{index:04}"),
        ));
    }
    let image = encode(&value);
    // Header, relations_bound, empty CTEs, QueryBlock, empty windows, empty projections.
    let start = 16 + 1 + 8 + 1 + 8 + 8;
    let mut expected = 1024u64.to_le_bytes().to_vec();
    for index in 0..1024 {
        expected.push(1);
        expected.extend(10u64.to_le_bytes());
        expected.extend(b"q.with.dot");
        expected.extend(5u64.to_le_bytes());
        expected.extend(format!("c{index:04}").as_bytes());
    }
    assert_eq!(&image[start..start + expected.len()], expected);
}

#[test]
fn mutation_cte_images_preserve_private_rewrite_metadata_and_selected_roles() {
    let mut value = query("WITH q AS (INSERT INTO t(a) VALUES (1) RETURNING a) SELECT * FROM q");
    let original = encode(&value);
    let CtePlanBody::Command(command) = &mut value.ctes[0].body else {
        panic!("command")
    };
    let CommandPlan::Insert(insert) = command.as_mut() else {
        panic!("insert")
    };
    insert.target_relation_bound = true;
    insert.relations_bound = true;
    insert.statement_privilege_subject = Some("statement.role".into());
    insert.target_privilege_subject = Some(crate::catalog::roles::RoleReference::Bound(
        std::sync::Arc::new(crate::catalog::roles::identity::RoleBinding::new(
            "bound.role".into(),
            42,
            [31; 16],
        )),
    ));
    let mut unresolved = uqa_core::memory::OwnedSet::new();
    unresolved.insert(2);
    unresolved.insert(7);
    let mut written = uqa_core::memory::OwnedSet::new();
    written.insert(0);
    insert.rule_values_input = Some(crate::plan::RuleValuesInput {
        width: 8,
        inline: false,
        defaults: vec![7, 2],
        unresolved,
        written,
    });
    insert
        .view_rule_insert_plans
        .push(crate::plan::ViewRuleInsertPlan {
            relation: "source.view".into(),
            supplied_columns: vec!["a".into()],
            input_columns: vec!["a".into(), "b".into()],
        });
    let encoded = encode(&value);
    assert_ne!(original, encoded);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&memory, &token, &token);
    let stored = crate::plan::copy::capture_stored_with_control(&value, &control).unwrap();
    assert_eq!(encode(&stored), encoded);
    drop(stored);
    assert_eq!(memory.used(), 0);
}
