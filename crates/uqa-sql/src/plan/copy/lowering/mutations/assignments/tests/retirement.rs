//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_mutation_fields_retire_every_suspended_prefix_and_denied_destination() {
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "INSERT INTO t(a[(SELECT 1):(SELECT 2)].x) VALUES (0)",
        "UPDATE t SET a[(SELECT 1)] = (SELECT 2), (b, c) = (SELECT 3, 4)",
        "INSERT INTO t VALUES (0) ON CONFLICT (a) WHERE a > (SELECT 4) DO UPDATE SET a[(SELECT 1)] = (SELECT 2) WHERE a > (SELECT 3)",
        "MERGE INTO t USING s ON true WHEN MATCHED AND (SELECT 1) > 0 THEN UPDATE SET a[(SELECT 2)] = (SELECT 3) WHEN NOT MATCHED THEN INSERT (a[(SELECT 5)]) VALUES ((SELECT 4))",
    ] {
        let (source, _, _) = fixture(sql);
        let source = Arc::new(source);
        let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(value);
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls - 1 {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut reader = input(source.clone(), &native, &work, &original, &invoking);
                for _ in 0..prefix { assert!(advance(&mut reader).unwrap().is_none()); }
                if cancellation != 0 {
                    if cancellation == 1 { original.cancel(); } else { invoking.cancel(); }
                    assert_eq!(advance(&mut reader).err().unwrap().sqlstate(), Some("57014"));
                    assert_eq!(advance(&mut reader).err().unwrap().sqlstate(), Some("XX001"));
                }
                assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
        }
        drop(sibling);
        for bytes in [0, 1, 128, 512, 1024] {
            let denied = MemoryBudget::new(bytes);
            let error = input(source.clone(), &denied, &work, &token, &token).read(&mut std::io::empty()).err().unwrap();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!((denied.used(), work.used()), (0, 0));
            assert!(denied.peak() <= bytes);
        }
    }
}

#[test]
fn retained_mutation_fields_retire_deep_prefixes_after_late_callback_failure() {
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        for sql in [
            "INSERT INTO t(a[1:(SELECT 99)]) VALUES (0)",
            "UPDATE t SET a[1] = (SELECT 99)",
            "INSERT INTO t VALUES (0) ON CONFLICT (a) WHERE a > (SELECT 99) DO UPDATE SET a = 1",
            "MERGE INTO t USING s ON true WHEN NOT MATCHED AND true THEN INSERT (a[(SELECT 99)]) VALUES (1)",
        ] {
            let (mut source, _, _) = fixture(sql);
            let mut deep = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 { deep = Expr::Not(Box::new(deep)); }
            match &mut source {
                Source::Targets(values) => {
                    let crate::ast::AssignmentStep::Slice { lower, .. } = &mut values[0].indirection[0] else { unreachable!() };
                    *lower = Some(Box::new(deep));
                }
                Source::Assignments(values) => {
                    let AssignmentTargets::Single(target) = &mut values[0].0 else { unreachable!() };
                    target.indirection[0] = crate::ast::AssignmentStep::Index(Box::new(deep));
                }
                Source::Conflict(value) => {
                    let crate::ast::OnConflictAction::Update { assignments, .. } = &mut value.action else { unreachable!() };
                    assignments[0].1 = deep;
                }
                Source::Merge(values) => {
                    let MergeWhen::InsertNotMatched { values, .. } = &mut values[0] else { unreachable!() };
                    values[0] = deep;
                }
            }
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let sibling = native.reserve(37).unwrap();
            let error = input(Arc::new(source), &native, &work, &token, &token).read(&mut std::io::empty()).err().unwrap();
            assert_eq!(error.to_string(), "internal error: rejected mutation query");
            assert_eq!((native.used(), work.used()), (37, 0));
            drop(sibling);
            assert_eq!(native.used(), 0);
        }
    }).unwrap().join().unwrap();
}
