//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_mutation_fields_preserve_ordinary_paths_positions_and_query_order() {
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "UPDATE t SET a[(SELECT 1):(SELECT 2)].x = (SELECT 3), (b, c) = (SELECT 4, 5)",
        "INSERT INTO t(a[(SELECT 1)]) VALUES (0)",
        "INSERT INTO t VALUES (0) ON CONFLICT (a) WHERE a > (SELECT 4) DO UPDATE SET a[(SELECT 1)] = (SELECT 2) WHERE a > (SELECT 3)",
        "INSERT INTO t VALUES (0) ON CONFLICT DO NOTHING",
        "MERGE INTO t USING s ON true WHEN MATCHED AND (SELECT 1) > 0 THEN UPDATE SET a[(SELECT 2)] = (SELECT 3) WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE AND (SELECT 4) > 0 THEN UPDATE SET a = (SELECT 5) WHEN NOT MATCHED BY SOURCE THEN DELETE WHEN NOT MATCHED AND (SELECT 6) > 0 THEN INSERT (a[(SELECT 8)]) OVERRIDING SYSTEM VALUE VALUES ((SELECT 7))",
        "MERGE INTO t USING s ON true WHEN MATCHED AND (SELECT 1) > 0 THEN DO NOTHING WHEN NOT MATCHED AND (SELECT 2) > 0 THEN DO NOTHING WHEN NOT MATCHED BY SOURCE AND (SELECT 3) > 0 THEN DO NOTHING",
    ] {
        let statement = parse_statement(sql);
        let ordinary = UnifiedPlan::lower(statement.clone());
        let control = ProductionControl::new(&native, &token, &token);
        let controlled = UnifiedPlan::lower_borrowed_for_rewrite(
            &statement, &crate::plan::NoRegisteredAggregates, &control,
        ).unwrap();
        let (UnifiedPlan::Command(ordinary), crate::plan::PlanRef::Command(controlled_plan)) =
            (ordinary, controlled.as_plan_ref()) else { panic!("mutation fixture"); };
        assert_eq!(serde_json::to_value(&*ordinary).unwrap(), serde_json::to_value(controlled_plan).unwrap(), "{sql}");
        drop(controlled);
        assert_eq!(native.used(), 0);
        let (expected, expected_queries) = expected_field(*ordinary);
        let source = source_field(statement);
        let ((value, memory, queries), polls) = complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        assert_eq!(serde_json::to_value(&*value).unwrap(), serde_json::to_value(expected).unwrap(), "{sql}");
        assert_eq!(&*queries, &expected_queries, "{sql}");
        assert!(polls > 1);
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        drop((memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_mutation_fields_keep_owned_names_and_nonconsecutive_source_positions() {
    let mut source = source_field(parse_statement("UPDATE t SET (a, b) = (SELECT 1, 2)"));
    let Source::Assignments(assignments) = &mut source else {
        unreachable!()
    };
    let AssignmentTargets::Multiple(group) = &mut assignments[0].0 else {
        unreachable!()
    };
    group.source_positions = vec![2, 0];
    group.source_width = 3;
    group.targets[0].column = "column_é🙂".repeat(4096);
    group.targets[0]
        .indirection
        .push(crate::ast::AssignmentStep::Field("field_é🙂".repeat(4096)));
    let source = Arc::new(source);
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let ((value, memory, queries), polls) =
        complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(source);
    let Field::Assignments(values) = &*value else {
        unreachable!()
    };
    let AssignmentTargets::Multiple(group) = &values[0].target else {
        unreachable!()
    };
    assert_eq!(group.source_positions, [2, 0]);
    assert_eq!(group.source_width, 3);
    assert_eq!(group.targets[0].column, "column_é🙂".repeat(4096));
    assert_eq!(
        group.targets[0].indirection,
        [crate::ast::AssignmentStep::Field("field_é🙂".repeat(4096))]
    );
    assert_eq!(&*queries, &[1]);
    assert!(polls > 20);
    drop(value);
    drop((memory, queries));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_mutation_vectors_bound_empty_and_wide_clause_work() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for count in [0, 2048] {
        for source in [
            Source::Targets(vec!["a".to_owned().into(); count]),
            Source::Assignments(vec![
                (
                    AssignmentTargets::from("a"),
                    Expr::Literal(Value::Int(1))
                );
                count
            ]),
            Source::Merge(vec![MergeWhen::NothingMatched { condition: None }; count]),
        ] {
            let ((value, memory, queries), polls) =
                complete(&mut input(Arc::new(source), &native, &work, &token, &token));
            assert!(polls > count.max(1));
            match &*value {
                Field::Targets(values) => assert_eq!(values.len(), count),
                Field::Assignments(values) => assert_eq!(values.len(), count),
                Field::Merge(values) => assert_eq!(values.len(), count),
                Field::Conflict(_) => unreachable!(),
            }
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop((memory, queries));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}
