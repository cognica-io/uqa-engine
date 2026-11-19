//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared field grammar preserves synchronous calling-stack ownership and pinned native continuations.

macro_rules! read_field {
    ($expression:expr) => {
        $expression
    };
}
macro_rules! await_field {
    ($expression:expr) => {{
        let future = std::pin::pin!($expression);
        future.await
    }};
}

// Collection fields stay inside the current native continuation. Synchronous drivers use the same body without introducing a nested future or another parser.
macro_rules! input_list {
    ($read:ident, $owner:expr, $allowance:expr, |$input:pat_param, $memory:pat_param| $body:block) => {{
        let count = $read!($owner.binary.count())?;
        let mut values = uqa_core::memory::BudgetedVec::new($owner.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            $read!($owner.checkpoint())?;
            let $input = &mut *$owner;
            let $memory = &mut *$allowance;
            let value: Result<_, SQLError> = $body;
            values.push(value?)?;
        }
        let (values, reservation) = values.into_parts();
        $allowance.absorb(reservation);
        Ok::<_, SQLError>(values)
    }};
}
