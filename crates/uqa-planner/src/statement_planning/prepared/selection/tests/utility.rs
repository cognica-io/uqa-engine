//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn session_utilities_keep_generic_plans_and_original_descriptors() {
    for sql in ["SHOW timezone", "FETCH ALL FROM c", "EXECUTE other($1)"] {
        let inputs = Inputs {
            mode: "force_custom_plan",
            ..Inputs::default()
        };
        inputs.fail_binding.set(true);
        let mut entry = inputs.entry.borrow().as_ref().unwrap().clone();
        let source = Arc::new(UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0)));
        entry.source_plan = source.clone();
        entry.logical_plan = source;
        entry.custom_plans = 0;
        entry.total_custom_cost = 0.0;
        entry.effective_search_path = Some(uqa_sql::catalog::resolution::EffectiveSearchPath {
            schemas: vec!["obsolete".into()],
            creation_namespace: Some("obsolete".into()),
        });
        let original_schema = entry.result_schema.clone();
        for iteration in 0..2 {
            let selected = select_entry(&inputs.context(), &entry, &[]).unwrap();
            assert!(selected.update.reanalyzed.is_none(), "{sql}");
            assert!(selected.update.custom_cost.is_none(), "{sql}");
            assert_eq!(has_parameter(&selected.plan), sql.contains("$1"));
            entry.record_execution(selected.update);
            assert_eq!(entry.generic_plans, iteration + 1);
            assert_eq!(entry.custom_plans, 0);
            assert_eq!(entry.result_schema, original_schema);
            if iteration == 1 {
                assert_eq!(*inputs.events.borrow(), ["mode"], "{sql}");
            }
            inputs.events.borrow_mut().clear();
            entry.invalidate();
            assert!(entry.plan.is_some(), "{sql}");
            assert!(!entry.needs_analysis, "{sql}");
        }
    }
}
