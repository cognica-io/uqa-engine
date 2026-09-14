//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered synonym resolution with optional bounds on the expanded JSON map.

use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
};

use crate::{descriptor::limits::check_limit, AnalysisResult};

pub(super) fn parse_synonym_body(body: &str) -> BTreeMap<String, Vec<String>> {
    parse(body, |_| Ok::<_, Infallible>(())).unwrap_or_else(|never| match never {})
}

pub(crate) fn parse_synonym_body_bounded(
    body: &str,
    maximum: usize,
) -> AnalysisResult<BTreeMap<String, Vec<String>>> {
    parse(body, |required| {
        check_limit("resolved synonym bytes", required, maximum)
    })
}

#[derive(Default)]
struct Terms<'a> {
    ordered: Vec<&'a str>,
    seen: BTreeSet<&'a str>,
}

struct Parser<'a, F> {
    entries: BTreeMap<&'a str, Terms<'a>>,
    bytes: usize,
    check: F,
}

fn parse<E>(
    body: &str,
    check: impl FnMut(usize) -> Result<(), E>,
) -> Result<BTreeMap<String, Vec<String>>, E> {
    let mut parser = Parser {
        entries: BTreeMap::new(),
        bytes: 2,
        check,
    };
    (parser.check)(parser.bytes)?;
    for line in body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if let Some((lhs, rhs)) = line.split_once("=>") {
            let rhs = members(rhs, 1);
            for term in members(lhs, 1) {
                parser.append(term, rhs.iter().copied())?;
            }
        } else {
            // Retaining a second occurrence preserves self-expansion and its original order.
            let members = members(line, 2);
            if members.len() < 2 {
                continue;
            }
            let mut seen = BTreeSet::new();
            for (index, term) in members.iter().enumerate() {
                if seen.insert(term) {
                    parser.append(
                        term,
                        members
                            .iter()
                            .enumerate()
                            .filter_map(|(other, term)| (index != other).then_some(*term)),
                    )?;
                }
            }
        }
    }
    Ok(parser
        .entries
        .into_iter()
        .map(|(term, entry)| {
            (
                term.into(),
                entry.ordered.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect())
}

impl<'a, E, F: FnMut(usize) -> Result<(), E>> Parser<'a, F> {
    fn append(&mut self, term: &'a str, values: impl Iterator<Item = &'a str>) -> Result<(), E> {
        if !self.entries.contains_key(term) {
            let bytes = self
                .bytes
                .saturating_add(quoted_len(term))
                .saturating_add(3 + usize::from(!self.entries.is_empty()));
            (self.check)(bytes)?;
            self.entries.insert(term, Terms::default());
            self.bytes = bytes;
        }
        let entry = self.entries.get_mut(term).expect("inserted synonym key");
        for value in values {
            if !entry.seen.contains(value) {
                let bytes = self
                    .bytes
                    .saturating_add(quoted_len(value))
                    .saturating_add(usize::from(!entry.ordered.is_empty()));
                (self.check)(bytes)?;
                entry.seen.insert(value);
                entry.ordered.push(value);
                self.bytes = bytes;
            }
        }
        Ok(())
    }
}

fn members(line: &str, maximum: u8) -> Vec<&str> {
    let mut counts = BTreeMap::<&str, u8>::new();
    line.split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .filter(|term| {
            let count = counts.entry(term).or_default();
            if *count == maximum {
                false
            } else {
                *count += 1;
                true
            }
        })
        .collect()
}

fn quoted_len(value: &str) -> usize {
    value.bytes().fold(2usize, |length, byte| {
        length.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_count_json_escapes_and_stop_before_expanding_the_whole_group() {
        let body = "a\\,b\"c,d\u{1}e,한글";
        let expected = parse_synonym_body(body);
        let length = serde_json::to_vec(&expected).unwrap().len();
        assert_eq!(parse_synonym_body_bounded(body, length).unwrap(), expected);
        assert!(parse_synonym_body_bounded(body, length - 1).is_err());
        let body = (0..10000)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_synonym_body_bounded(&body, body.len()).is_err());
    }

    #[test]
    fn duplicate_equivalence_members_preserve_self_expansion_and_first_insertion_order() {
        let body = "a,b,a,c,a,b\na,b => c,d,c\nempty =>\nsolo\n";
        assert_eq!(
            parse_synonym_body(body),
            BTreeMap::from([
                (
                    "a".into(),
                    vec!["b".into(), "a".into(), "c".into(), "d".into()]
                ),
                (
                    "b".into(),
                    vec!["a".into(), "c".into(), "b".into(), "d".into()]
                ),
                ("c".into(), vec!["a".into(), "b".into()]),
                ("empty".into(), vec![]),
            ])
        );
        let body = std::iter::repeat_n("same", 10000)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_synonym_body(&body),
            BTreeMap::from([("same".into(), vec!["same".into()])])
        );
    }
}
