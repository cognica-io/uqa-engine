//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared heap transitions retain the next comparison without borrowing a caller's elements.

use std::cmp::Ordering;

pub(super) enum Step {
    Checkpoint,
    Compare(usize, usize),
    Swap(usize, usize),
}

enum Phase {
    Initial,
    Build,
    Sift,
    Children,
    Root,
    Exchange,
    Extract,
    Take,
    Done,
}

pub(super) struct HeapOrder {
    end: usize,
    build: usize,
    root: usize,
    child: usize,
    phase: Phase,
}

impl HeapOrder {
    pub fn new(len: usize) -> Self {
        Self {
            end: len,
            build: len / 2,
            root: 0,
            child: 0,
            phase: Phase::Initial,
        }
    }

    pub fn next(&mut self, order: Ordering) -> Option<Step> {
        loop {
            match self.phase {
                Phase::Initial => {
                    self.phase = Phase::Build;
                    return Some(Step::Checkpoint);
                }
                Phase::Build => {
                    if self.build == 0 {
                        self.phase = Phase::Extract;
                    } else {
                        self.build -= 1;
                        self.root = self.build;
                        self.phase = Phase::Sift;
                    }
                }
                Phase::Sift => {
                    if self.root < self.end / 2 {
                        self.child = self.root * 2 + 1;
                        self.phase = Phase::Children;
                        return Some(Step::Checkpoint);
                    }
                    self.phase = Phase::Build;
                }
                Phase::Children => {
                    if self.child + 1 < self.end {
                        self.phase = Phase::Root;
                        return Some(Step::Compare(self.child, self.child + 1));
                    }
                    self.phase = Phase::Exchange;
                    return Some(Step::Compare(self.root, self.child));
                }
                Phase::Root => {
                    if order.is_lt() {
                        self.child += 1;
                    }
                    self.phase = Phase::Exchange;
                    return Some(Step::Compare(self.root, self.child));
                }
                Phase::Exchange => {
                    if order.is_lt() {
                        let root = self.root;
                        self.root = self.child;
                        self.phase = Phase::Sift;
                        return Some(Step::Swap(root, self.child));
                    }
                    self.phase = Phase::Build;
                }
                Phase::Extract => {
                    if self.end > 1 {
                        self.end -= 1;
                        self.root = 0;
                        self.phase = Phase::Take;
                    } else {
                        self.phase = Phase::Done;
                    }
                    return Some(Step::Checkpoint);
                }
                Phase::Take => {
                    self.phase = Phase::Sift;
                    return Some(Step::Swap(0, self.end));
                }
                Phase::Done => return None,
            }
        }
    }
}
