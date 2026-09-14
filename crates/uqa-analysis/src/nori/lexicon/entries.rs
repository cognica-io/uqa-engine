//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible enumeration of every accepted surface in UTF-16 lexical order.

use super::Lexicon;
use crate::nori::io::vector;
use crate::nori::DictionaryResult;

struct Position {
    node: u32,
    next_arc: u32,
    entered: bool,
}

pub(crate) struct Entries<'a> {
    lexicon: &'a Lexicon,
    pending: Vec<Position>,
    path: Vec<u16>,
    failed: bool,
}

impl<'a> Entries<'a> {
    pub(super) fn new(lexicon: &'a Lexicon) -> Self {
        Self {
            lexicon,
            pending: vec![Position {
                node: lexicon.root,
                next_arc: 0,
                entered: false,
            }],
            path: Vec::new(),
            failed: false,
        }
    }

    fn next_surface(&mut self) -> DictionaryResult<Option<Vec<u16>>> {
        while let Some(position) = self.pending.last_mut() {
            let node = &self.lexicon.nodes[position.node as usize];
            if !position.entered {
                position.entered = true;
                if node.terminal {
                    let mut surface = vector(self.path.len())?;
                    surface.extend_from_slice(&self.path);
                    return Ok(Some(surface));
                }
            }
            if position.next_arc == node.arc_count {
                let _ = self.pending.pop();
                let _ = self.path.pop();
                continue;
            }
            let arc = &self.lexicon.arcs[(node.first_arc + position.next_arc) as usize];
            position.next_arc += 1;
            self.path.try_reserve(1)?;
            self.path.push(arc.label);
            self.pending.try_reserve(1)?;
            self.pending.push(Position {
                node: arc.target,
                next_arc: 0,
                entered: false,
            });
        }
        Ok(None)
    }
}

impl Iterator for Entries<'_> {
    type Item = DictionaryResult<Vec<u16>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_surface() {
            Ok(value) => value.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
