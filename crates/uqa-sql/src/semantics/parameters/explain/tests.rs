//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn capture_reads_only_planning_settings_and_preserves_display_and_boot_rules() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let reads = std::cell::RefCell::new(Vec::new());
    let output = capture(
        &|definition, control| {
            reads.borrow_mut().push(definition.name);
            Ok(Some(match definition.name {
                "work_mem" => control.copy_text("65536")?,
                "search_path" => control.copy_text("custom, public")?,
                _ => definition.boot_setting_with_control(control)?,
            }))
        },
        &control,
    )
    .unwrap();
    assert_eq!(
        &*reads.borrow(),
        &[
            "enable_indexonlyscan",
            "plan_cache_mode",
            "search_path",
            "work_mem"
        ]
    );
    assert_eq!(
        output.as_slice(),
        &[
            ("search_path", "custom, public".into()),
            ("work_mem", "64MB".into())
        ]
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn capture_stops_at_the_original_allowance_or_cancellation_and_releases_partial_settings() {
    let huge = "x".repeat(1024 * 1024);
    for cancel in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let allocations = allocation_counter::measure(|| {
            let error = capture(
                &|definition, control| {
                    if definition.name == "search_path" {
                        if cancel {
                            invoking.cancel();
                        }
                        return Ok(Some(control.copy_text(&huge)?));
                    }
                    Ok(Some(control.copy_text("off")?))
                },
                &control,
            )
            .unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some(if cancel { "57014" } else { "53200" })
            );
        });
        assert!(allocations.bytes_max < 16 * 1024, "{allocations:?}");
        assert_eq!(budget.used(), 0);
    }
}
