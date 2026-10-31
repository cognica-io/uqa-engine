//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn initialization_stops_when_a_callback_cancels_either_scope() {
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = MemoryBudget::new(4096);
        let expression = ScalarExpr::Row(vec![
            ScalarExpr::Literal(Value::Int(1)),
            ScalarExpr::ScalarSubquery(0),
        ]);
        let mut visited = 0;
        let error = visit_with_control(
            &expression,
            &mut |_| {
                visited += 1;
                if visited == 2 {
                    if cancel_original {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                }
                Ok(())
            },
            &ProductionControl::new(&memory, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(visited, 2);
        assert_eq!(memory.used(), 0);
    }
}
