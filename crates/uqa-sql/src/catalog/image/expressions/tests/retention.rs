//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{RetainedInputLiteral, WindowCallModifiers, WindowSpec},
    ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral},
};
use std::sync::Arc;

struct Chain(Option<Expr>);
impl Drop for Chain {
    fn drop(&mut self) {
        let mut next = self.0.take();
        while let Some(value) = next {
            next = match value {
                Expr::Binary { lhs, rhs, .. } => {
                    drop(rhs);
                    Some(*lhs)
                }
                Expr::UnaryMinus(child) => Some(*child),
                _ => None,
            };
        }
    }
}

fn chain(depth: usize) -> Chain {
    let mut value = Expr::Star;
    for _ in 0..depth {
        value = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(value),
            rhs: Box::new(Expr::Default),
        };
    }
    Chain(Some(value))
}

#[test]
fn definition_readers_admit_deep_continuations_and_retire_every_failed_read() {
    let owned = chain(4096);
    let value = owned.0.as_ref().unwrap();
    let token = CancellationToken::new();
    let ample = MemoryBudget::new(16 << 20);
    let expected = collect(value.image_reader(&ample, &token).unwrap(), 20000);
    assert_eq!(expected.len(), 16 + 3 * 4096 + 1);
    assert_eq!(&expected[16..18], &[13, 6]);
    let peak = ample.peak();
    assert!(peak > 0);
    assert_eq!(ample.used(), 0);
    for limit in [0, 1, size_of::<Frame<'_>>() - 1] {
        let memory = MemoryBudget::new(limit);
        let mut reader = value.image_reader(&memory, &token).unwrap();
        let mut buffer = [0; 31];
        loop {
            match reader.read(&mut buffer) {
                Ok(count) => assert_ne!(count, 0),
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"));
                    break;
                }
            }
        }
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut buffer).is_err());
        assert_eq!(memory.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    assert_eq!(
        collect(value.image_reader(&exact, &token).unwrap(), 13),
        expected
    );
    assert_eq!(exact.used(), 0);
}

#[test]
fn definition_export_is_iterative_on_small_stacks_and_borrows_large_names() {
    let owned = chain(10000);
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let bytes = collect(
                owned
                    .0
                    .as_ref()
                    .unwrap()
                    .image_reader(&memory, &token)
                    .unwrap(),
                127,
            );
            assert_eq!(bytes.len(), 30017);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
    let value = Expr::Column("α-original".repeat(65536).into());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    let mut count = 0;
    let mut buffer = vec![0; 20000];
    let allocations = allocation_counter::measure(|| {
        let mut reader = value.image_reader(&memory, &token).unwrap();
        loop {
            let n = reader.read(&mut buffer).unwrap();
            assert!(n <= WINDOW);
            if n == 0 {
                break;
            }
            count += n;
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(count, 16 + 1 + 8 + "α-original".len() * 65536);
}

#[test]
fn definition_readers_retire_cancelled_state_and_keep_completion_terminal() {
    let value = Expr::Array(vec![Expr::Star; 10000]);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let mut reader = value.image_reader(&memory, &token).unwrap();
    assert_eq!(reader.read(&mut [0; 64]).unwrap(), 64);
    assert!(memory.used() > 0);
    token.cancel();
    assert!(reader.read(&mut [0; 64]).is_err());
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut []).is_err());
    assert!(value.image_reader(&memory, &token).is_err());
    let token = CancellationToken::new();
    let mut reader = Expr::Star.image_reader(&memory, &token).unwrap();
    assert_eq!(reader.read(&mut [0; 64]).unwrap(), 17);
    token.cancel();
    assert_eq!(reader.read(&mut [0; 64]).unwrap(), 0);
}

#[derive(Debug)]
struct Unavailable {
    unwind: bool,
}
impl ConstantValueSource for Unavailable {
    fn read(&self, _index: u64) -> Result<Value, SQLError> {
        panic!("image output must not materialize an external source")
    }
    fn resident_value(&self, _index: u64) -> Option<&Value> {
        assert!(!self.unwind, "injected resident-value unwind");
        None
    }
}

#[test]
fn invalid_or_unwound_definition_sources_retire_without_executing_callbacks() {
    for unwind in [false, true] {
        let value = Expr::Array(vec![
            Expr::Star,
            Expr::RetainedLiteral(Box::new(RetainedInputLiteral(RetainedScalarLiteral::new(
                RetainedConstantValue::new(Arc::new(Unavailable { unwind }), 0),
                None,
                None,
                None,
            )))),
        ]);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let mut reader = value.image_reader(&memory, &token).unwrap();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.read(&mut [0; 200])));
        if unwind {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut [0; 200]).is_err());
    }
    let value = Expr::Array(vec![Expr::WindowCall {
        name: "f".into(),
        args: vec![],
        spec: Box::new(WindowSpec {
            raw_definition: None,
            definition: None,
            reference: None,
            partition_by: vec![],
            order_by: vec![],
            frame: None,
        }),
        filter: None,
        modifiers: WindowCallModifiers::default(),
    }]);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&memory, &token).unwrap();
    let error = reader.read(&mut [0; 200]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut [0; 200]).is_err());
}
