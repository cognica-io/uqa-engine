//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#![cfg(not(target_os = "emscripten"))]

use super::*;

#[derive(Default)]
struct Observer {
    checks: Vec<usize>,
}

impl RegexResources for Observer {
    fn reserve(&mut self, _: usize) -> bool {
        true
    }

    fn release(&mut self, _: usize) {}

    fn cancel_requested(&mut self) -> bool {
        false
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.checks.push(depth);
        false
    }
}

#[test]
fn stack_callback_refuses_the_hard_limit_before_consulting_the_resource_hook() {
    let mut context = MemoryContext::unlinked();
    let mut resources = Observer::default();
    let limit = MAX_STACK_DEPTH / 2;
    let mut operation = Operation::new(&raw mut context, &mut resources, limit);
    let pointer = operation.as_raw();
    assert_eq!(
        uqa_pg_regex_callback_stack(pointer, MAX_STACK_DEPTH + 1),
        CALLBACK_REFUSED
    );
    assert_eq!(
        uqa_pg_regex_callback_stack(pointer, limit + 1),
        CALLBACK_RESTART
    );
    assert_eq!(uqa_pg_regex_callback_stack(pointer, limit), CALLBACK_OK);
    assert_eq!(resources.checks, [limit]);
}
