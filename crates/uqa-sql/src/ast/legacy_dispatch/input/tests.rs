//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBinding, FunctionDispatch};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
};

type Decoder = ImageDecoder<bool>;

fn declaration(statements: Vec<Statement>) -> CreateFunction {
    let Statement::CreateFunction(mut definition) =
        crate::compile("CREATE FUNCTION public.f(x integer DEFAULT 1) RETURNS integer RETURN 1")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    definition.body = FunctionBody::Statements(statements);
    *definition
}

fn decoder(
    definition: Arc<CreateFunction>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Decoder, ImageIoError> {
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        definition
            .has_legacy_serialized_dispatches_from_input(&original, &mut reader.input())
            .await
    })
}

struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("routine dispatch observation only borrows native syntax")
    }
}
fn advance(input: &mut Decoder, steps: usize) -> Result<Option<bool>, ImageIoError> {
    input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
}
fn complete(input: &mut Decoder, steps: usize) -> Result<(bool, usize), ImageIoError> {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, polls));
        }
    }
    panic!("routine dispatch observation stopped progressing")
}

fn original(definition: &CreateFunction) -> bool {
    let mut definition = definition.clone();
    let mut changed = false;
    for parameter in &mut definition.params {
        if let Some(default) = &mut parameter.default {
            changed |= default.upgrade_legacy_serialized_dispatches();
        }
    }
    if let FunctionBody::Statements(statements) = &mut definition.body {
        for statement in statements {
            changed |= statement.upgrade_legacy_serialized_dispatches();
        }
    }
    changed
}

fn compare(definition: CreateFunction, expected: bool) {
    assert_eq!(original(&definition), expected);
    let before = serde_json::to_value(&definition).unwrap();
    let definition = Arc::new(definition);
    for steps in [1, 7, 128] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(Arc::clone(&definition), &memory, &token, &token).unwrap();
        assert_eq!(complete(&mut input, steps).unwrap().0, expected);
        assert_eq!(memory.used(), 0);
        assert!(advance(&mut input, steps).is_err());
        assert_eq!(serde_json::to_value(&*definition).unwrap(), before);
    }
}

fn legacy_expression() -> Expr {
    let Statement::Select(mut query) = crate::compile("SELECT __to_hex_int4(1)").unwrap().remove(0)
    else {
        unreachable!()
    };
    query.projections.remove(0).expr
}

fn replace_marker(value: &mut serde_json::Value, selected: usize, index: &mut usize) {
    match value {
        serde_json::Value::Object(fields) => {
            if fields.get("name").and_then(serde_json::Value::as_str) == Some("migration_probe") {
                if *index == selected {
                    fields.insert("name".into(), "__to_hex_int4".into());
                }
                *index += 1;
            }
            for value in fields.values_mut() {
                replace_marker(value, selected, index);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                replace_marker(value, selected, index);
            }
        }
        _ => {}
    }
}

#[test]
fn retained_routine_dispatch_observation_matches_original_complete_statement_migration() {
    let sql = [
        "SELECT migration_probe(1), ARRAY[migration_probe(2)], ROW(migration_probe(3)), -migration_probe(4), migration_probe(5) IS NULL, migration_probe(6)::text",
        "SELECT CASE migration_probe(1) WHEN migration_probe(2) THEN migration_probe(3) ELSE migration_probe(4) END",
        "SELECT migration_probe(1) IN (migration_probe(2), migration_probe(3)), EXISTS(SELECT migration_probe(4)), (SELECT migration_probe(5))",
        "SELECT migration_probe(1) IN (SELECT migration_probe(2)) WHERE migration_probe(3) > 0 AND NOT migration_probe(4) = 0",
        "SELECT DISTINCT ON (migration_probe(1)) migration_probe(2) FROM (VALUES (migration_probe(3))) v(x) GROUP BY migration_probe(4) HAVING migration_probe(5) > 0 ORDER BY migration_probe(6) LIMIT migration_probe(7) OFFSET migration_probe(8)",
        "SELECT sum(migration_probe(1) ORDER BY migration_probe(2)) FILTER (WHERE migration_probe(3) > 0)",
        "SELECT sum(migration_probe(1)) FILTER (WHERE migration_probe(2) > 0) OVER (PARTITION BY migration_probe(3) ORDER BY migration_probe(4) ROWS BETWEEN migration_probe(5) PRECEDING AND migration_probe(6) FOLLOWING)",
        "SELECT sum(1) OVER w WINDOW w AS (PARTITION BY migration_probe(1) ORDER BY migration_probe(2) ROWS migration_probe(3) PRECEDING)",
        "SELECT migration_probe(1) GROUP BY GROUPING SETS ((migration_probe(2)), (migration_probe(3)))",
        "SELECT migration_probe(1) UNION SELECT migration_probe(2) ORDER BY 1 LIMIT migration_probe(3) OFFSET migration_probe(4)",
        "SELECT 1 FROM (SELECT migration_probe(1)) a JOIN (VALUES (migration_probe(2))) b(x) ON migration_probe(3) > 0",
        "SELECT 1 FROM ROWS FROM (generate_series(migration_probe(1), 2), generate_series(1, migration_probe(2)))",
        "WITH a AS (SELECT migration_probe(1)), b AS (INSERT INTO t VALUES (migration_probe(2)) RETURNING migration_probe(3)) SELECT migration_probe(4)",
        "WITH a AS (UPDATE t SET x = migration_probe(1) RETURNING migration_probe(2)), b AS (DELETE FROM t WHERE migration_probe(3) > 0 RETURNING migration_probe(4)) SELECT 1",
        "INSERT INTO t (x[migration_probe(1)]) VALUES (migration_probe(2)) ON CONFLICT ((migration_probe(x))) WHERE migration_probe(3) > 0 DO UPDATE SET x[migration_probe(4):migration_probe(5)] = migration_probe(6) WHERE migration_probe(7) > 0 RETURNING migration_probe(8)",
        "INSERT INTO t SELECT migration_probe(1)",
        "UPDATE t SET x[migration_probe(1)] = migration_probe(2), (a,b) = (SELECT migration_probe(3), migration_probe(4)) FROM (SELECT migration_probe(5)) s WHERE migration_probe(6) > 0 RETURNING migration_probe(7)",
        "DELETE FROM t USING (SELECT migration_probe(1)) s WHERE migration_probe(2) > 0 RETURNING migration_probe(3)",
        "MERGE INTO t USING (SELECT migration_probe(1)) s ON migration_probe(2) > 0 WHEN MATCHED AND migration_probe(3) > 0 THEN UPDATE SET x[migration_probe(4)] = migration_probe(5) WHEN NOT MATCHED AND migration_probe(6) > 0 THEN INSERT (x) VALUES (migration_probe(7)) RETURNING migration_probe(8)",
        "CREATE DOMAIN d AS integer DEFAULT migration_probe(1) CHECK (migration_probe(VALUE) > 0)",
        "ALTER DOMAIN d SET DEFAULT migration_probe(1)",
        "ALTER DOMAIN d ADD CHECK (migration_probe(VALUE) > 0)",
        "CREATE VIEW v AS SELECT migration_probe(1)",
        "CREATE MATERIALIZED VIEW v AS SELECT migration_probe(1)",
        "CREATE TABLE v AS SELECT migration_probe(1)",
        "DECLARE c CURSOR FOR SELECT migration_probe(1)",
        "EXPLAIN SELECT migration_probe(1)",
        "PREPARE p AS SELECT migration_probe(1)",
        "EXECUTE p(migration_probe(1))",
        "CALL p(migration_probe(1))",
        "VALUES (migration_probe(1))",
        "CREATE FUNCTION f(x integer DEFAULT migration_probe(1)) RETURNS integer RETURN migration_probe(x)",
        "CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW WHEN (migration_probe(1) > 0) EXECUTE FUNCTION f()",
        "CREATE RULE r AS ON INSERT TO t WHERE migration_probe(1) > 0 DO ALSO SELECT migration_probe(2)",
    ];
    for sql in sql {
        let statement = crate::compile(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
            .remove(0);
        compare(declaration(vec![statement.clone()]), false);
        let json = serde_json::to_value(statement).unwrap();
        let mut count = 0;
        replace_marker(&mut json.clone(), usize::MAX, &mut count);
        assert!(count > 0, "{sql}");
        for selected in 0..count {
            let mut value = json.clone();
            replace_marker(&mut value, selected, &mut 0);
            compare(
                declaration(vec![serde_json::from_value(value).unwrap()]),
                true,
            );
        }
    }
}

#[test]
fn retained_routine_dispatch_observation_preserves_source_bodies_and_user_bindings() {
    let mut definition = declaration(Vec::new());
    definition.body = FunctionBody::Source("__to_hex_int4(1); this is not parsed".repeat(10_000));
    compare(definition.clone(), false);
    let mut expression = legacy_expression();
    let Expr::Func { binding, .. } = &mut expression else {
        unreachable!()
    };
    let mut user = FunctionBinding::dispatched(FunctionDispatch::ToHexInt4);
    user.dispatch = None;
    user.builtin = false;
    *binding = Some(user);
    definition.params[0].default = Some(expression.clone());
    compare(definition.clone(), false);
    let Expr::Func { binding, .. } = definition.params[0].default.as_mut().unwrap() else {
        unreachable!()
    };
    *binding = None;
    compare(definition, true);
}

#[test]
fn retained_routine_dispatch_observation_preserves_range_slice_cte_and_table_function_predicates() {
    let legacy = legacy_expression();
    for length in 0..18 {
        let mut function = declaration(Vec::new());
        function.params[0].default = Some(Expr::Func {
            name: "array".into(),
            binding: Some(FunctionBinding::dispatched(FunctionDispatch::ArraySlices)),
            args: vec![Expr::Literal(uqa_core::Value::Null); length],
            distinct: false,
            order_by: Vec::new(),
            order_syntax: crate::ast::FunctionOrderSyntax::default(),
            filter: None,
        });
        compare(function, length > 1 && length % 2 == 1);
    }
    for distinct in [false, true] {
        let mut function = declaration(Vec::new());
        function.params[0].default = Some(Expr::Func {
            name: "range".into(),
            binding: Some(FunctionBinding::dispatched(
                FunctionDispatch::BetweenSymmetric,
            )),
            args: vec![Expr::Literal(uqa_core::Value::Int(1)); 3],
            distinct,
            order_by: Vec::new(),
            order_syntax: crate::ast::FunctionOrderSyntax::default(),
            filter: None,
        });
        compare(function, !distinct);
    }
    for mark_value in [false, true] {
        let Statement::Select(mut query) = crate::compile(
            "WITH RECURSIVE t(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM t WHERE x < 2) CYCLE x SET c USING p SELECT x FROM t",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let cycle = query.with[0].cycle.as_mut().unwrap();
        if mark_value {
            cycle.mark_value = legacy.clone();
        } else {
            cycle.mark_default = legacy.clone();
        }
        compare(declaration(vec![Statement::Select(query)]), true);
    }
    for sql in [
        "SELECT * FROM generate_series(1, 2)",
        "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(2, 3))",
    ] {
        for builtin in [false, true] {
            let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
                unreachable!()
            };
            let mut binding = FunctionBinding::dispatched(FunctionDispatch::ToHexInt4);
            binding.name = "pg_catalog.generate_series".into();
            binding.dispatch = None;
            binding.builtin = builtin;
            binding.argument_types = vec!["integer".into(); 3];
            match query.from.as_mut().unwrap() {
                FromClause::Function {
                    binding: target, ..
                } => *target = Some(binding),
                FromClause::FunctionGroup { functions, .. } => functions[1].binding = Some(binding),
                _ => unreachable!(),
            }
            compare(declaration(vec![Statement::Select(query)]), builtin);
        }
    }
    for clause in [
        MergeWhen::DeleteMatched {
            condition: Some(legacy.clone()),
        },
        MergeWhen::DeleteNotMatchedBySource {
            condition: Some(legacy.clone()),
        },
        MergeWhen::NothingMatched {
            condition: Some(legacy.clone()),
        },
        MergeWhen::NothingNotMatched {
            condition: Some(legacy.clone()),
        },
        MergeWhen::NothingNotMatchedBySource {
            condition: Some(legacy.clone()),
        },
        MergeWhen::UpdateNotMatchedBySource {
            condition: None,
            assignments: vec![("x".into(), legacy)],
        },
    ] {
        let Statement::Merge(mut merge) =
            crate::compile("MERGE INTO t USING s ON true WHEN MATCHED THEN DO NOTHING")
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        merge.when_clauses = vec![clause];
        compare(declaration(vec![Statement::Merge(merge)]), true);
    }
}

#[test]
fn retained_routine_dispatch_observation_bounds_empty_children_and_native_depth() {
    let mut definition = declaration(vec![Statement::Values {
        rows: vec![Vec::new(); 4096],
    }]);
    definition.params = vec![definition.params[0].clone(); 4096];
    for parameter in &mut definition.params {
        parameter.default = None;
    }
    let definition = Arc::new(definition);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (legacy, polls) = complete(
        &mut decoder(definition, &memory, &token, &token).unwrap(),
        1,
    )
    .unwrap();
    assert!(!legacy);
    assert!(polls >= 8192);
    assert!(
        memory.peak() < 16_384,
        "width must not allocate one frame per sibling"
    );
    assert_eq!(memory.used(), 0);

    let mut definition = declaration(Vec::new());
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut expression = Expr::Literal(uqa_core::Value::Int(1));
            for _ in 0..4096 {
                expression = Expr::Not(Box::new(expression));
            }
            definition.params[0].default = Some(expression);
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            assert!(
                !complete(
                    &mut decoder(Arc::new(definition), &memory, &token, &token).unwrap(),
                    1
                )
                .unwrap()
                .0
            );
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_routine_dispatch_observation_retires_every_pending_frame_and_typed_failure() {
    let definition = Arc::new(declaration(crate::compile("WITH a AS (SELECT 1) SELECT CASE WHEN 1 > 0 THEN (SELECT 2) ELSE 3 END FROM (VALUES(4)) v(x)").unwrap()));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (_, polls) = complete(
        &mut decoder(Arc::clone(&definition), &memory, &token, &token).unwrap(),
        1,
    )
    .unwrap();
    for stop in 0..polls {
        for cancelled in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input =
                decoder(Arc::clone(&definition), &memory, &original, &invoking).unwrap();
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            if let Some(original_owner) = cancelled {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut input, 1),
                    Err(ImageIoError::Cancelled(_))
                ));
                assert_eq!(memory.used(), 0);
                assert!(advance(&mut input, 1).is_err());
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [0, 8, 128] {
        let memory = MemoryBudget::new(limit);
        let result = decoder(Arc::clone(&definition), &memory, &token, &token)
            .and_then(|mut input| complete(&mut input, 1));
        assert!(matches!(result, Err(ImageIoError::Memory(_))));
        assert_eq!(memory.used(), 0);
    }
}

mod markers;
