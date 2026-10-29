//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{JoinExecutionStrategy, MutableSourcePlan, OwnedProjection, SourcePlan};

#[test]
fn generated_query_source_and_projection_keep_exact_constructor_owners() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        let expression =
            OwnedScalarExpr::column(control.copy_text("column").unwrap(), None, &control).unwrap();
        let query = MutableQueryPlan::projected(
            std::iter::once(OwnedProjection {
                expression,
                alias: Some(control.copy_text("output").unwrap()),
            }),
            &control,
        )
        .unwrap();
        let query = query
            .replace_source(Some(MutableSourcePlan::unit(&control).unwrap()), &control)
            .unwrap();
        result = Some(
            MutableSourcePlan::subquery(
                query,
                Some(control.copy_text("derived").unwrap()),
                control
                    .finish(Vec::new(), control.empty_reservation())
                    .unwrap(),
                None,
                false,
                &control,
            )
            .unwrap(),
        );
    });
    let result = result.unwrap();
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    assert_eq!(result.reserved_bytes(), budget.used());
    let before = budget.used();
    let mut selected = None;
    let extraction =
        allocation_counter::measure(|| selected = Some(result.into_query(&control).unwrap()));
    let selected = selected.unwrap();
    assert_eq!(extraction.count_total, 0);
    assert_eq!(
        before as i64 + extraction.bytes_current,
        budget.used() as i64
    );
    assert_eq!(selected.reserved_bytes(), budget.used());
    let RelationalPlan::QueryBlock(block) = &selected.root else {
        unreachable!()
    };
    assert!(
        matches!(&block.from, Some(SourcePlan::Values { rows, .. }) if rows == &[Vec::<ScalarExpr>::new()])
    );
    drop(selected);
    assert_eq!(budget.used(), 0);
}

fn join() -> SourcePlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT * FROM a JOIN b ON a.id=b.id")
            .unwrap()
            .remove(0),
    ) else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        unreachable!()
    };
    block.from.unwrap()
}

#[test]
fn join_shell_reuses_original_boxes_and_moves_independent_predicate() {
    let original = join();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let source = original.copy_for_rewrite(&control).unwrap();
    let SourcePlan::Join {
        left, right, on, ..
    } = &*source
    else {
        unreachable!()
    };
    let boxes = (
        std::ptr::from_ref(left.as_ref()),
        std::ptr::from_ref(right.as_ref()),
    );
    let ScalarExpr::Binary { lhs, .. } = on.as_ref().unwrap() else {
        unreachable!()
    };
    let predicate_child = std::ptr::from_ref(lhs.as_ref());
    let before = budget.used();
    let mut rebuilt = None;
    let allocation = allocation_counter::measure(|| {
        let parts = source.split_join(&control).unwrap();
        rebuilt = Some(
            parts
                .shell
                .assemble(
                    parts.right,
                    parts.left,
                    parts.on,
                    JoinExecutionStrategy::default(),
                    &control,
                )
                .unwrap(),
        );
    });
    let rebuilt = rebuilt.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), before);
    assert_eq!(rebuilt.reserved_bytes(), before);
    let SourcePlan::Join {
        left, right, on, ..
    } = &*rebuilt
    else {
        unreachable!()
    };
    assert_eq!(
        boxes,
        (
            std::ptr::from_ref(left.as_ref()),
            std::ptr::from_ref(right.as_ref())
        )
    );
    assert!(matches!(left.as_ref(), SourcePlan::Table { name, .. } if name == "b"));
    let ScalarExpr::Binary { lhs, .. } = on.as_ref().unwrap() else {
        unreachable!()
    };
    assert_eq!(predicate_child, std::ptr::from_ref(lhs.as_ref()));
    drop(rebuilt);
    assert_eq!(budget.used(), 0);
}

#[test]
fn join_assembly_releases_all_private_inputs_on_either_cancellation() {
    let original = join();
    for invoking in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original_token = CancellationToken::new();
        let invoking_token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original_token, &invoking_token);
        let parts = original
            .copy_for_rewrite(&control)
            .unwrap()
            .split_join(&control)
            .unwrap();
        if invoking {
            invoking_token.cancel();
        } else {
            original_token.cancel();
        }
        let error = parts
            .shell
            .assemble(
                parts.left,
                parts.right,
                parts.on,
                JoinExecutionStrategy::default(),
                &control,
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn privilege_subject_replacement_keeps_exact_query_constructor_ownership() {
    use crate::catalog::roles::RoleReference;
    for sql in ["SELECT 1", "VALUES (1)", "SELECT 1 UNION SELECT 2"] {
        let statement = crate::compile(sql).unwrap().remove(0);
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement.clone()) else {
            panic!("query fixture")
        };
        for copied in [false, true] {
            let budget = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let owner = if copied {
                query.copy_for_rewrite(&control).unwrap()
            } else {
                let crate::plan::MutableExecutablePlan::Query(query) =
                    UnifiedPlan::lower_borrowed_for_rewrite(
                        &statement,
                        &crate::plan::NoRegisteredAggregates,
                        &control,
                    )
                    .unwrap()
                else {
                    panic!("query fixture")
                };
                query
            };
            let initial = budget.used();
            let role = RoleReference::from("view_owner");
            let mut owner = Some(owner);
            let installed = allocation_counter::measure(|| {
                owner = Some(
                    owner
                        .take()
                        .unwrap()
                        .set_privilege_subject(
                            Some(role.clone_with_control(&control).unwrap()),
                            &control,
                        )
                        .unwrap(),
                );
            });
            assert_eq!(
                initial as i64 + installed.bytes_current,
                budget.used() as i64
            );
            assert_eq!(
                owner.as_ref().unwrap().relation_privilege_subject.as_ref(),
                Some(&role)
            );
            let before = budget.used();
            let removed = allocation_counter::measure(|| {
                owner = Some(
                    owner
                        .take()
                        .unwrap()
                        .set_privilege_subject(None, &control)
                        .unwrap(),
                );
            });
            assert_eq!(before as i64 + removed.bytes_current, budget.used() as i64);
            assert_eq!(budget.used(), initial);
            assert_eq!(owner.as_ref().unwrap().reserved_bytes(), budget.used());
            drop(owner);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn privilege_column_removal_releases_copied_and_added_dependencies() {
    for copied in [false, true] {
        let mut source = fixture();
        let RelationalPlan::QueryBlock(block) = &mut source.root else {
            panic!("query block")
        };
        if copied {
            block
                .privilege_columns
                .insert(crate::ColumnIdentity::unqualified("retained"));
        }
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let source = source.copy_for_rewrite(&control).unwrap();
        let source = source.clear_privilege_columns(&control).unwrap();
        let initial = budget.used();
        let mut source = Some(source);
        let changed = allocation_counter::measure(|| {
            let column =
                crate::ColumnIdentity::from_parts_with_control(Some("t"), "retained", &control)
                    .unwrap();
            let added = source
                .take()
                .unwrap()
                .include_privilege_column(column, &control)
                .unwrap();
            let column =
                crate::ColumnIdentity::from_parts_with_control(Some("t"), "retained", &control)
                    .unwrap();
            let added = added.include_privilege_column(column, &control).unwrap();
            source = Some(added.clear_privilege_columns(&control).unwrap());
        });
        assert_eq!(changed.bytes_current, 0);
        assert_eq!(budget.used(), initial);
        assert_eq!(source.as_ref().unwrap().reserved_bytes(), budget.used());
        drop(source);
        assert_eq!(budget.used(), 0);
    }
}
