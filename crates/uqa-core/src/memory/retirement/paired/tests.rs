//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{MemoryBudget, MemoryReservation, TreeRetirement};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

type Children = Option<Box<(Node, Node)>>;
struct Node {
    children: Children,
    payload: Vec<MemoryReservation>,
    retired: Arc<AtomicUsize>,
}

impl PairedRetirementNode for Node {
    type Local = ();
    fn children_mut(&mut self) -> &mut Children {
        &mut self.children
    }
    fn retire_local(&mut self, (): &mut ()) -> bool {
        self.payload.pop().is_none()
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        if self.children.is_some() {
            drop(TreeRetirement::take(&mut self.children));
        }
        self.retired.fetch_add(1, Ordering::Relaxed);
    }
}

fn tree(depth: usize, budget: &MemoryBudget, retired: &Arc<AtomicUsize>) -> Children {
    let node = |children| Node {
        children,
        payload: (0..3).map(|_| budget.reserve(1).unwrap()).collect(),
        retired: retired.clone(),
    };
    let mut current = None;
    for level in 0..depth {
        current = Some(if level.is_multiple_of(2) {
            Box::new((node(current), node(None)))
        } else {
            Box::new((node(None), node(current)))
        });
    }
    current
}

#[test]
fn paired_tree_retirement_reuses_links_and_bounds_local_steps() {
    let budget = MemoryBudget::new(1 << 20);
    let retired = Arc::new(AtomicUsize::new(0));
    let sibling = budget.reserve(37).unwrap();
    let mut source = tree(12, &budget, &retired);
    let mut retirement = TreeRetirement::take(&mut source);
    assert!(source.is_none());
    let mut steps = 0;
    loop {
        let before = budget.used();
        let complete = retirement.advance();
        assert!(before - budget.used() <= 1);
        steps += 1;
        if complete {
            break;
        }
        assert!(steps < 1000);
    }
    assert!(retirement.advance());
    assert_eq!(retired.load(Ordering::Relaxed), 24);
    assert_eq!(budget.used(), sibling.bytes());
    assert_eq!(
        allocation_counter::measure(|| drop(retirement)).count_total,
        0
    );
}

#[test]
fn paired_tree_retirement_releases_every_suspended_prefix_without_allocating() {
    let budget = MemoryBudget::new(1 << 20);
    let retired = Arc::new(AtomicUsize::new(0));
    let sibling = budget.reserve(37).unwrap();
    let mut complete = TreeRetirement::new(tree(4, &budget, &retired));
    let mut steps = 1;
    while !complete.advance() {
        steps += 1;
    }
    drop(complete);
    for prefix in 0..steps {
        retired.store(0, Ordering::Relaxed);
        let mut retirement = TreeRetirement::new(tree(4, &budget, &retired));
        for _ in 0..prefix {
            assert!(!retirement.advance());
        }
        assert_eq!(
            allocation_counter::measure(|| drop(retirement)).count_total,
            0
        );
        assert_eq!(retired.load(Ordering::Relaxed), 8);
        assert_eq!(budget.used(), sibling.bytes());
    }
}

#[test]
fn paired_tree_retirement_drops_deep_native_nodes_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let budget = MemoryBudget::new(1 << 20);
            let retired = Arc::new(AtomicUsize::new(0));
            let source = tree(8192, &budget, &retired);
            assert_eq!(allocation_counter::measure(|| drop(source)).count_total, 0);
            assert_eq!(retired.load(Ordering::Relaxed), 16_384);
            assert_eq!(budget.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
