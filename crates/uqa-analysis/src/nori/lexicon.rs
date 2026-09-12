//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A minimal acyclic UTF-16 transducer whose outputs are lexical ranks.

use super::error::{check_limit, invalid};
use super::io::{vector, Reader};
use super::DictionaryResult;

#[cfg(any(test, feature = "nori-tools"))]
mod builder;
#[cfg(any(test, feature = "nori-tools"))]
pub(super) use builder::Builder;
#[cfg(any(test, feature = "nori-tools"))]
mod entries;

#[derive(Debug)]
struct Node {
    first_arc: u32,
    arc_count: u32,
    suffixes: u32,
    terminal: bool,
}

#[derive(Debug)]
struct Arc {
    label: u16,
    target: u32,
    output: u32,
}

#[derive(Debug)]
pub(super) struct Lexicon {
    nodes: Vec<Node>,
    arcs: Vec<Arc>,
    root: u32,
}

impl Lexicon {
    #[cfg(any(test, feature = "nori-tools"))]
    pub fn entries(&self) -> entries::Entries<'_> {
        entries::Entries::new(self)
    }

    pub fn len(&self) -> usize {
        self.nodes[self.root as usize].suffixes as usize
    }

    pub fn cursor(&self) -> Cursor<'_> {
        Cursor {
            lexicon: self,
            node: self.root,
            rank: 0,
        }
    }

    pub fn lookup(&self, text: impl IntoIterator<Item = u16>) -> Option<u32> {
        let mut cursor = self.cursor();
        for label in text {
            cursor.advance(label)?;
        }
        cursor.rank()
    }

    pub fn decode(reader: &mut Reader<'_>, maximum_word_length: usize) -> DictionaryResult<Self> {
        let root = reader.u32()?;
        let node_count = reader.count(5)?;
        let arc_count = reader.count(6)?;
        let required = node_count
            .checked_mul(5)
            .and_then(|bytes| arc_count.checked_mul(6)?.checked_add(bytes))
            .ok_or_else(|| reader.invalid("lexicon size overflow"))?;
        if node_count == 0 || root as usize != node_count - 1 || required > reader.remaining() {
            return Err(reader.invalid("invalid lexicon dimensions"));
        }
        let mut nodes: Vec<Node> = vector(node_count)?;
        let mut next_arc = 0_u32;
        for _ in 0..node_count {
            let count = reader.u32()?;
            let terminal = match reader.u8()? {
                0 => false,
                1 => true,
                _ => return Err(reader.invalid("invalid terminal flag")),
            };
            nodes.push(Node {
                first_arc: next_arc,
                arc_count: count,
                suffixes: u32::from(terminal),
                terminal,
            });
            next_arc = next_arc
                .checked_add(count)
                .ok_or_else(|| reader.invalid("arc count overflow"))?;
        }
        if next_arc as usize != arc_count {
            return Err(reader.invalid("arc counts differ"));
        }
        let mut arcs = vector(arc_count)?;
        for index in 0..node_count {
            let mut suffixes = nodes[index].suffixes;
            for _ in 0..nodes[index].arc_count {
                let label = reader.u16()?;
                let target = reader.u32()?;
                if target as usize >= index {
                    return Err(reader.invalid("arc target is not an earlier state"));
                }
                arcs.push(Arc {
                    label,
                    target,
                    output: suffixes,
                });
                suffixes = suffixes
                    .checked_add(nodes[target as usize].suffixes)
                    .ok_or_else(|| reader.invalid("accepted word count overflow"))?;
            }
            nodes[index].suffixes = suffixes;
        }
        let lexicon = Self { nodes, arcs, root };
        lexicon.validate(maximum_word_length)?;
        Ok(lexicon)
    }

    fn validate(&self, maximum_word_length: usize) -> DictionaryResult<()> {
        let mut next_arc = 0;
        let mut lengths: Vec<usize> = vector(self.nodes.len())?;
        for (index, node) in self.nodes.iter().enumerate() {
            if node.first_arc as usize != next_arc {
                return Err(invalid(
                    "lexicon",
                    "overlapping or noncontiguous arc ranges",
                ));
            }
            let end = next_arc
                .checked_add(node.arc_count as usize)
                .ok_or_else(|| invalid("lexicon", "arc range overflow"))?;
            let arcs = self
                .arcs
                .get(next_arc..end)
                .ok_or_else(|| invalid("lexicon", "arc range exceeds section"))?;
            let mut suffixes = u32::from(node.terminal);
            let mut previous = None;
            let mut length = 0;
            for arc in arcs {
                if arc.target as usize >= index || previous.is_some_and(|label| arc.label <= label)
                {
                    return Err(invalid("lexicon", "cyclic, unordered, or invalid arc"));
                }
                let child = &self.nodes[arc.target as usize];
                if child.suffixes == 0 || arc.output != suffixes {
                    return Err(invalid("lexicon", "invalid lexical rank output"));
                }
                suffixes = suffixes
                    .checked_add(child.suffixes)
                    .ok_or_else(|| invalid("lexicon", "accepted word count overflow"))?;
                length = length.max(lengths[arc.target as usize] + 1);
                previous = Some(arc.label);
            }
            if suffixes != node.suffixes {
                return Err(invalid("lexicon", "accepted word count differs"));
            }
            lengths.push(length);
            next_arc = end;
        }
        if next_arc != self.arcs.len() {
            return Err(invalid("lexicon", "unreferenced arcs"));
        }
        check_limit(
            "UTF-16 units per surface",
            lengths[self.root as usize],
            maximum_word_length,
        )?;
        self.validate_utf16()
    }

    fn validate_utf16(&self) -> DictionaryResult<()> {
        let mut visited = vector(self.nodes.len())?;
        visited.resize(self.nodes.len(), 0_u8);
        let mut pending = vec![(self.root, false)];
        while let Some((index, low_required)) = pending.pop() {
            let mask = if low_required { 2 } else { 1 };
            if visited[index as usize] & mask != 0 {
                continue;
            }
            visited[index as usize] |= mask;
            let node = &self.nodes[index as usize];
            if low_required && node.terminal {
                return Err(invalid("lexicon", "surface ends inside a surrogate pair"));
            }
            let start = node.first_arc as usize;
            for arc in &self.arcs[start..start + node.arc_count as usize] {
                let next_low = match (low_required, arc.label) {
                    (true, 0xdc00..=0xdfff) => false,
                    (true, _) | (false, 0xdc00..=0xdfff) => {
                        return Err(invalid("lexicon", "surface contains an unpaired surrogate"));
                    }
                    (false, 0xd800..=0xdbff) => true,
                    _ => false,
                };
                pending.try_reserve(1)?;
                pending.push((arc.target, next_low));
            }
        }
        if visited.contains(&0) {
            return Err(invalid("lexicon", "unreachable states"));
        }
        Ok(())
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn encode(&self, output: &mut super::io::Writer) -> DictionaryResult<()> {
        output.u32(self.root)?;
        output.count(self.nodes.len())?;
        output.count(self.arcs.len())?;
        for node in &self.nodes {
            output.u32(node.arc_count)?;
            output.u8(u8::from(node.terminal))?;
        }
        for arc in &self.arcs {
            output.u16(arc.label)?;
            output.u32(arc.target)?;
        }
        Ok(())
    }
}

pub(super) struct Cursor<'a> {
    lexicon: &'a Lexicon,
    node: u32,
    rank: u32,
}

impl Cursor<'_> {
    pub fn advance(&mut self, label: u16) -> Option<()> {
        let node = &self.lexicon.nodes[self.node as usize];
        let start = node.first_arc as usize;
        let arcs = &self.lexicon.arcs[start..start + node.arc_count as usize];
        let index = arcs.binary_search_by_key(&label, |arc| arc.label).ok()?;
        let arc = &arcs[index];
        self.rank += arc.output;
        self.node = arc.target;
        Some(())
    }

    pub fn rank(&self) -> Option<u32> {
        self.lexicon.nodes[self.node as usize]
            .terminal
            .then_some(self.rank)
    }
}

#[cfg(test)]
mod tests;
