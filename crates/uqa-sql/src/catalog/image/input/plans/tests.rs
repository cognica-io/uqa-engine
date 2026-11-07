//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::CancellationToken;

mod resources;
mod scopes;
mod sources;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(value) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture")
    };
    *value
}

fn encode(value: &QueryPlan) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let output = collect(value.image_reader(&memory, &token).unwrap());
    assert_eq!(memory.used(), 0);
    output
}

fn collect(mut reader: crate::catalog::image::DefinitionImageReader<'_>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0; 127];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn values_wire(scalar: &[u8]) -> Vec<u8> {
    let mut bytes = b"UQAQRY01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(0); // Original relation-binding state.
    bytes.extend(0u64.to_le_bytes()); // Empty WITH list.
    bytes.push(2); // VALUES root.
    bytes.extend(1u64.to_le_bytes()); // One row.
    bytes.extend(1u64.to_le_bytes()); // One cell.
    bytes.extend(scalar);
    bytes.extend(0u64.to_le_bytes()); // Empty scalar-query arena.
    bytes
}

#[test]
fn native_query_input_matches_independent_wire_and_rejects_framing() {
    let bytes = values_wire(&[0]); // Native Star constructor.
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(!value.relations_bound);
    assert!(value.ctes.is_empty());
    let RelationalPlan::Values { rows, subqueries } = &value.root else {
        panic!("VALUES")
    };
    assert_eq!(rows, &[vec![ScalarExpr::Star]]);
    assert!(subqueries.is_empty());
    assert_eq!(encode(&value), bytes);
    drop(value);
    assert_eq!(memory.used(), 0);
    for count in [0u64, 2, u64::MAX] {
        let mut malformed = bytes.clone();
        malformed[8..16].copy_from_slice(&count.to_le_bytes());
        assert!(QueryPlan::read_image(&mut malformed.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut malformed = bytes.clone();
    malformed.push(0);
    assert!(QueryPlan::read_image(&mut malformed.as_slice(), &memory, &token).is_err());
    malformed = bytes;
    malformed[25] = 255;
    assert!(QueryPlan::read_image(&mut malformed.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_query_input_preserves_compiler_queries_sources_and_mutation_ctes() {
    for sql in [
        "SELECT DISTINCT a + 1 AS x, (SELECT 7) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY x DESC NULLS FIRST LIMIT 4 OFFSET 1",
        "SELECT a, sum(b) FROM t GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING sum(b) > 2",
        "SELECT sum(a) OVER (w ROWS BETWEEN 2 PRECEDING AND CURRENT ROW EXCLUDE TIES) FROM t WINDOW w AS (ORDER BY a)",
        "SELECT * FROM t FOR NO KEY UPDATE OF t SKIP LOCKED",
        "(SELECT 1 UNION ALL SELECT 2) INTERSECT SELECT 3 ORDER BY 1 LIMIT 4",
        "VALUES (1, 'a'), (2, 'b')",
        "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
        "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH q AS (INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a) SELECT * FROM q",
        "WITH q AS (UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
        "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a) SELECT * FROM q",
        "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN DO NOTHING WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE AND t.a > 0 THEN DO NOTHING WHEN NOT MATCHED BY SOURCE THEN DELETE WHEN NOT MATCHED THEN DO NOTHING RETURNING t.a) SELECT * FROM q",
    ] {
        let original = query(sql);
        let bytes = encode(&original);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let restored = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap_or_else(|error| panic!("{sql}: {error}"));
        // Native fields are compared against compiler output, independently of the writer's field traversal.
        assert_eq!(serde_json::to_value(&*restored).unwrap(), serde_json::to_value(&original).unwrap(), "{sql}");
        assert_eq!(encode(&restored), bytes, "{sql}");
        drop(restored);
        assert_eq!(memory.used(), 0, "{sql}");
    }
}

#[test]
fn native_query_input_releases_every_truncated_native_parent() {
    let original = query("WITH q AS (UPDATE t SET a[1:2] = ARRAY[3, 4] FROM (SELECT 1 AS a) u WHERE t.a[1] = u.a RETURNING t.a) SELECT * FROM q UNION ALL SELECT ARRAY[5, 6]");
    let bytes = encode(&original);
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    for length in 0..bytes.len() {
        assert!(
            QueryPlan::read_image(&mut &bytes[..length], &memory, &token).is_err(),
            "prefix {length}"
        );
        assert_eq!(memory.used(), 0, "prefix {length}");
    }
}

#[test]
fn native_query_input_retains_selected_roles_columns_and_rewrite_state() {
    use crate::{
        catalog::roles::{identity::RoleBinding, RoleReference},
        ColumnIdentity,
    };
    let mut original = query("WITH q AS (INSERT INTO t(a) VALUES (1) RETURNING a) SELECT * FROM q");
    let RelationalPlan::QueryBlock(block) = &mut original.root else {
        panic!("block")
    };
    block
        .privilege_columns
        .insert(ColumnIdentity::unqualified("z"));
    block
        .privilege_columns
        .insert(ColumnIdentity::qualified("q.with.dot", "a.with.dot"));
    let CtePlanBody::Command(command) = &mut original.ctes[0].body else {
        panic!("command")
    };
    let CommandPlan::Insert(value) = command.as_mut() else {
        panic!("insert")
    };
    value.target_relation_bound = true;
    value.relations_bound = true;
    value.statement_privilege_subject = Some("statement.role".into());
    value.target_privilege_subject = Some(RoleReference::Bound(std::sync::Arc::new(
        RoleBinding::new("bound.role".into(), 42, [31; 16]),
    )));
    let mut unresolved = uqa_core::memory::OwnedSet::new();
    unresolved.insert(2);
    unresolved.insert(7);
    let mut written = uqa_core::memory::OwnedSet::new();
    written.insert(0);
    value.rule_values_input = Some(crate::plan::RuleValuesInput {
        width: 8,
        inline: false,
        defaults: vec![7, 2],
        unresolved,
        written,
    });
    value
        .view_rule_insert_plans
        .push(crate::plan::ViewRuleInsertPlan {
            relation: "source.view".into(),
            supplied_columns: vec!["a".into()],
            input_columns: vec!["a".into(), "b".into()],
        });
    value.view_rule_returning = Some(crate::plan::ViewRuleReturningPlan {
        relation: "source.view".into(),
        target_qualifier: "source".into(),
        returning: vec![crate::plan::ProjectionPlan {
            expr: ScalarExpr::ScalarSubquery(0),
            alias: None,
        }],
        aliases: crate::ast::ReturningAliases::default(),
        subqueries: vec![query("VALUES (9)")],
    });
    let bytes = encode(&original);
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let restored = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        serde_json::to_value(&*restored).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(encode(&restored), bytes);
    let CtePlanBody::Command(command) = &restored.ctes[0].body else {
        panic!("command")
    };
    let CommandPlan::Insert(value) = command.as_ref() else {
        panic!("insert")
    };
    let Some(RoleReference::Bound(role)) = &value.target_privilege_subject else {
        panic!("role")
    };
    let role = role.clone();
    drop(restored);
    assert_eq!(role.name, "bound.role");
    assert_eq!(
        memory.used(),
        size_of::<RoleBinding>() + role.name.capacity()
    );
    drop(role);
    assert_eq!(memory.used(), 0);
}
