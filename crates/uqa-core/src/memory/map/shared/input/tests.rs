//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::MemoryBudget;
use std::{
    collections::BTreeMap,
    future::Future,
    task::{Context, Poll, Waker},
};

mod insertion;

struct Query(usize);
struct Input {
    visited: usize,
    fail_at: Option<usize>,
}
impl MapKeyInput<Query, usize> for Input {
    type Error = usize;

    async fn checkpoint(&mut self) -> Result<(), usize> {
        let mut ready = false;
        std::future::poll_fn(|_| {
            if ready {
                Poll::Ready(())
            } else {
                ready = true;
                Poll::Pending
            }
        })
        .await;
        if self.fail_at == Some(self.visited) {
            return Err(self.visited);
        }
        self.visited += 1;
        Ok(())
    }

    async fn compare(&mut self, query: &Query, stored: &usize) -> Result<Ordering, usize> {
        <Self as MapKeyInput<Query, usize>>::checkpoint(self).await?;
        Ok(query.0.cmp(stored))
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    for _ in 0..128 {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
    panic!("retained lookup stopped progressing")
}

#[test]
fn retained_map_input_lookup_preserves_independent_order_and_selected_generations() {
    let memory = MemoryBudget::new(1 << 20);
    let mut source = BudgetedSharedMap::new(&memory);
    let expected = (0..128usize)
        .map(|key| (key * 2, key * 3))
        .collect::<BTreeMap<_, _>>();
    for (&key, &value) in &expected {
        source.try_insert(key, value).unwrap();
    }
    let selected = source.clone();
    source.try_insert(64, 9999).unwrap();
    let used = memory.used();
    for key in 0..257 {
        let mut input = Input {
            visited: 0,
            fail_at: None,
        };
        assert_eq!(
            finish(selected.get_from(&Query(key), &mut input)),
            Ok(expected.get(&key))
        );
        assert!(
            input.visited <= 10,
            "lookup must retain the balanced tree path"
        );
        assert_eq!(memory.used(), used);
    }
    drop(source);
    assert_eq!(
        finish(selected.get_from(
            &Query(64),
            &mut Input {
                visited: 0,
                fail_at: None
            }
        )),
        Ok(expected.get(&64))
    );
    drop(selected);
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_map_input_lookup_failure_or_abandonment_never_mutates_the_owner() {
    let memory = MemoryBudget::new(1 << 20);
    let mut source = BudgetedSharedMap::new(&memory);
    for key in 0..128usize {
        source.try_insert(key, key).unwrap();
    }
    let used = memory.used();
    let before = source
        .iter()
        .map(|(&key, &value)| (key, value))
        .collect::<Vec<_>>();
    let mut complete = Input {
        visited: 0,
        fail_at: None,
    };
    assert_eq!(
        finish(source.get_from(&Query(0), &mut complete)),
        Ok(Some(&0))
    );
    for fail_at in 0..complete.visited {
        let mut input = Input {
            visited: 0,
            fail_at: Some(fail_at),
        };
        assert_eq!(finish(source.get_from(&Query(0), &mut input)), Err(fail_at));
        assert_eq!(memory.used(), used);
    }
    let mut input = Input {
        visited: 0,
        fail_at: None,
    };
    {
        let future = source.get_from(&Query(0), &mut input);
        let mut future = std::pin::pin!(future);
        assert!(future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
    }
    assert_eq!(
        source
            .iter()
            .map(|(&key, &value)| (key, value))
            .collect::<Vec<_>>(),
        before
    );
    assert_eq!(memory.used(), used);
    drop(source);
    assert_eq!(memory.used(), 0);
}
