//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken,
};

#[test]
fn owned_parameterization_preserves_binding_order_across_queries_and_mutations() {
    let cases = [
        "SELECT v + $1, (SELECT v), ARRAY[v], CASE WHEN v > 0 THEN v ELSE $1 END",
        "WITH q AS (SELECT v AS n) SELECT v FROM q WHERE n = v ORDER BY v LIMIT v",
        "SELECT sum(v) OVER (PARTITION BY v ORDER BY v ROWS v PRECEDING) FROM (VALUES(v)) t(n)",
        "INSERT INTO t(a) VALUES(v) ON CONFLICT(a) DO UPDATE SET a = v WHERE a = v RETURNING v",
        "UPDATE t SET (a,b) = (v,v) FROM (VALUES(v)) q(n) WHERE a = v RETURNING v",
        "DELETE FROM t USING (VALUES(v)) q(n) WHERE a = v RETURNING v",
        "MERGE INTO t USING (VALUES(v)) q(n) ON a = v WHEN MATCHED THEN UPDATE SET a = v WHEN NOT MATCHED THEN INSERT(a) VALUES(v) RETURNING v",
        "SELECT v UNION ALL SELECT v ORDER BY v",
        "EXPLAIN SELECT v WHERE v = $1",
        "CALL p(v, $1)",
    ];
    for sql in cases {
        let statement = crate::compile(sql).unwrap().remove(0);
        let before = serde_json::to_value(&statement).unwrap();
        let ordinary = parameterize_statement_variables(
            &statement,
            &mut Variables(4),
            VariableConflict::Error,
            &mut |_, _, names| Ok(vec![VariableSiteResolution::Variable; names.len()]),
            &|_: &str| false,
        )
        .unwrap();
        let memory = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let owned = parameterize_statement_variables_owned(
            &statement,
            &mut Variables(4),
            VariableConflict::Error,
            &mut |_, _, names, control| {
                let mut output = ProductionVec::new(*control);
                for _ in 0..names.len() {
                    output.push_copy(VariableSiteResolution::Variable)?;
                }
                Ok(output.finish()?)
            },
            &|_: &str| false,
            &control,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&*owned.plan.finish(&control).unwrap()).unwrap(),
            serde_json::to_value(&ordinary.plan).unwrap(),
            "{sql}"
        );
        assert_eq!(
            format!("{:?}", *owned.references),
            format!("{:?}", ordinary.references),
            "{sql}"
        );
        assert_eq!(
            format!("{:?}", *owned.parameters),
            format!("{:?}", ordinary.parameters),
            "{sql}"
        );
        assert_eq!(serde_json::to_value(&statement).unwrap(), before);
        drop(owned.references);
        drop(owned.parameters);
        assert_eq!(memory.used(), 0, "{sql}");
    }
}

#[test]
fn owned_parameterization_rejects_before_callbacks_and_releases_partial_inputs() {
    let statement = crate::compile("SELECT v, $1, (SELECT v)")
        .unwrap()
        .remove(0);
    for cancel in 0..3 {
        let memory = MemoryBudget::new(if cancel == 0 { 0 } else { 1024 * 1024 });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel == 1 {
            original.cancel();
        }
        if cancel == 2 {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let result = parameterize_statement_variables_owned(
            &statement,
            &mut Variables(4),
            VariableConflict::Error,
            &mut |_, _, _, _| panic!("admission precedes catalog callback"),
            &|_: &str| false,
            &control,
        );
        assert_eq!(
            result.err().unwrap().sqlstate(),
            Some(if cancel == 0 { "53200" } else { "57014" })
        );
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let result = parameterize_statement_variables_owned(
        &statement,
        &mut Variables(4),
        VariableConflict::Error,
        &mut |_, _, _, _| Err(SQLError::Internal("site analysis failed".into())),
        &|_: &str| false,
        &control,
    );
    assert_eq!(
        result.err().unwrap().to_string(),
        "internal error: site analysis failed"
    );
    assert_eq!(memory.used(), 0);
}
