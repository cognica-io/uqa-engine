//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A path-compressed byte prefix index. Edges borrow labels by index into the
//! owned mapping keys, so long common prefixes and tails need no per-byte states.

use crate::{source::EditBuilder, work::Work, AnalysisError, AnalysisResult};
use std::collections::{btree_map::Entry, BTreeMap};

#[derive(Debug, Clone, Copy)]
struct Edge {
    rule: usize,
    start: usize,
    end: usize,
    target: usize,
}

#[derive(Debug)]
pub(super) struct PrefixIndex {
    rules: Vec<(String, String)>,
    edges: BTreeMap<(usize, u8), Edge>,
    terminals: BTreeMap<usize, usize>,
    starts: [bool; 256],
}

impl PrefixIndex {
    pub(super) async fn prepare(
        rules: Vec<(String, String)>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Self> {
        let mut edges = BTreeMap::new();
        let mut terminals = BTreeMap::new();
        let mut starts = [false; 256];
        let mut next_node = 1;
        for (rule, (key, _)) in rules.iter().enumerate() {
            work.records(rule).await?;
            let key = key.as_bytes();
            starts[usize::from(key[0])] = true;
            let mut node = 0;
            let mut offset = 0;
            while offset < key.len() {
                let mut entry = match edges.entry((node, key[offset])) {
                    Entry::Vacant(vacant) => {
                        node = next_node;
                        next_node += 1;
                        vacant.insert(Edge {
                            rule,
                            start: offset,
                            end: key.len(),
                            target: node,
                        });
                        break;
                    }
                    Entry::Occupied(entry) => entry,
                };
                let edge = *entry.get();
                let label = &rules[edge.rule].0.as_bytes()[edge.start..edge.end];
                let mut prefix = CommonPrefix::new(label, &key[offset..]);
                while prefix.pending() {
                    work.checkpoint().await?;
                    prefix.advance();
                }
                let common = prefix.matched;
                offset += common;
                if common == label.len() {
                    node = edge.target;
                    continue;
                }
                node = next_node;
                next_node += 1;
                let split = edge.start + common;
                entry.insert(Edge {
                    end: split,
                    target: node,
                    ..edge
                });
                edges.insert(
                    (node, label[common]),
                    Edge {
                        start: split,
                        ..edge
                    },
                );
            }
            terminals.insert(node, rule);
        }
        Ok(Self {
            rules,
            edges,
            terminals,
            starts,
        })
    }

    pub(super) fn apply(&self, input: &str, edits: &mut EditBuilder<'_, '_>) -> AnalysisResult<()> {
        let input = input.as_bytes();
        let mut start = 0;
        let mut next_check = 0;
        while start < input.len() {
            if start >= next_check {
                edits.check()?;
                next_check = start.saturating_add(1024);
            }
            if !self.starts[usize::from(input[start])] {
                start += 1;
                continue;
            }
            let mut end = start;
            let mut node = 0;
            let mut longest = None;
            while let Some(&byte) = input.get(end) {
                let Some(edge) = self.edges.get(&(node, byte)) else {
                    break;
                };
                let label = &self.rules[edge.rule].0.as_bytes()[edge.start..edge.end];
                let mut prefix = CommonPrefix::new(label, &input[end..]);
                while prefix.pending() {
                    edits.check()?;
                    prefix.advance();
                }
                let n = prefix.matched;
                if n != label.len() {
                    break;
                }
                end += n;
                node = edge.target;
                if let Some(&rule) = self.terminals.get(&node) {
                    longest = Some((end, rule));
                }
            }
            if let Some((end, rule)) = longest {
                edits.edit(start..end, std::iter::once(self.rules[rule].1.as_str()))?;
                start = end;
            } else {
                start += 1;
            }
        }
        Ok(())
    }
}

struct CommonPrefix<'a> {
    left: &'a [u8],
    right: &'a [u8],
    matched: usize,
    end: usize,
}

impl<'a> CommonPrefix<'a> {
    fn new(left: &'a [u8], right: &'a [u8]) -> Self {
        Self {
            left,
            right,
            matched: 0,
            end: left.len().min(right.len()),
        }
    }

    fn pending(&self) -> bool {
        self.matched < self.end
    }

    fn advance(&mut self) {
        let end = self.matched.saturating_add(1024).min(self.end);
        let a = &self.left[self.matched..end];
        let b = &self.right[self.matched..end];
        if a == b {
            self.matched = end;
        } else {
            self.matched += a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .expect("different blocks");
            self.end = self.matched;
        }
    }
}
