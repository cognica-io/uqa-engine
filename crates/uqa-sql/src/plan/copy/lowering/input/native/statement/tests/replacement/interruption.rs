//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn updates(
    replacement: RetainedExecutablePlan,
    replace_source: bool,
    control: &ProductionControl<'_>,
) -> Replacements {
    if replace_source {
        Replacements {
            source: Some(source(replacement, control)),
            ..Default::default()
        }
    } else {
        Replacements {
            query: Some(QueryMetadataUpdate {
                root: Some(replacement.try_into().unwrap()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

#[test]
fn native_replacement_retires_every_suspended_prefix_and_both_cancellation_scopes() {
    let budget = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = budget.reserve(37).unwrap();
    let original_plan = statement("SELECT old_col FROM (SELECT old_source) s");
    let replacement = statement("SELECT new_col FROM (SELECT new_source) s");
    for replace_source in [false, true] {
        let mut completed = false;
        for prefix in 0..2000 {
            for cancel in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&budget, &original, &invoking);
                let destination = construct(&original_plan, &budget, &work, &original);
                let next = construct(&replacement, &budget, &work, &original);
                let mut reader = input(
                    destination,
                    updates(next, replace_source, &control),
                    &budget,
                    &work,
                    &original,
                    &invoking,
                );
                for _ in 0..prefix {
                    if let Some((plan, visited)) = advance(&mut reader).unwrap() {
                        assert_eq!(
                            visited,
                            if replace_source {
                                ["old_col", "new_source"]
                            } else {
                                ["new_col", "new_source"]
                            }
                        );
                        assert_eq!(allocation_counter::measure(|| drop(plan)).count_total, 0);
                        completed = true;
                        break;
                    }
                }
                if !completed && cancel != 0 {
                    if cancel == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                }
                assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
                assert_eq!((budget.used(), work.used()), (sibling.bytes(), 0));
            }
            if completed {
                assert!(prefix > 20);
                break;
            }
        }
        assert!(completed);
    }
}

#[test]
fn native_replacement_denial_and_missing_constructor_leave_destination_unchanged() {
    let budget = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    let original = statement("SELECT old_col FROM original");
    for replace_source in [false, true] {
        for native in [false, true] {
            let mut destination = construct(&original, &budget, &work, &token);
            let before = serde_json::to_value(query(&destination)).unwrap();
            let mut observer = if native {
                let next = construct(
                    &statement("SELECT new_col FROM replacement"),
                    &budget,
                    &work,
                    &token,
                );
                updates(next, replace_source, &control)
            } else {
                let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
                    crate::compile("SELECT new_col FROM replacement")
                        .unwrap()
                        .remove(0),
                ) else {
                    unreachable!()
                };
                if replace_source {
                    let RelationalPlan::QueryBlock(block) = &mut query.root else {
                        unreachable!()
                    };
                    Replacements {
                        source: Some(
                            block
                                .from
                                .take()
                                .unwrap()
                                .copy_for_rewrite(&control)
                                .unwrap()
                                .into(),
                        ),
                        ..Default::default()
                    }
                } else {
                    Replacements {
                        query: Some(QueryMetadataUpdate {
                            root: Some(query.copy_for_rewrite(&control).unwrap().into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }
                }
            };
            let held = native.then(|| budget.reserve(budget.available()).unwrap());
            let error = crate::catalog::work::inline(destination.apply_metadata_from(
                &mut observer,
                &control,
                &mut (),
            ))
            .unwrap_err();
            if native {
                assert_eq!(error.sqlstate(), Some("53200"));
            } else {
                assert!(error.to_string().contains("constructor retirement owner"));
            }
            assert!(observer.query.is_none() && observer.source.is_none());
            assert_eq!(serde_json::to_value(query(&destination)).unwrap(), before);
            drop(observer);
            assert_eq!(
                allocation_counter::measure(|| drop(destination)).count_total,
                0
            );
            drop(held);
            assert_eq!((budget.used(), work.used()), (sibling.bytes(), 0));
        }
    }
}
