//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared ordered lookup tables permit cancellation within long key comparisons.

use std::{borrow::Cow, cmp::Ordering, collections::BTreeMap};

use crate::AnalysisResult;

#[cfg(test)]
mod tests;

fn compare(
    left: &str,
    right: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Ordering> {
    for (left, right) in left
        .as_bytes()
        .chunks(1024)
        .zip(right.as_bytes().chunks(1024))
    {
        poll()?;
        match left.cmp(right) {
            Ordering::Equal => {}
            order => return Ok(order),
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn find<'a>(
    length: usize,
    mut key: impl FnMut(usize) -> &'a str,
    term: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Option<usize>> {
    poll()?;
    let (mut left, mut right) = (0, length);
    while left < right {
        poll()?;
        let middle = left + (right - left) / 2;
        match compare(key(middle), term, poll)? {
            Ordering::Less => left = middle + 1,
            Ordering::Greater => right = middle,
            Ordering::Equal => return Ok(Some(middle)),
        }
    }
    Ok(None)
}

pub(crate) fn find_word(
    words: &[Cow<'_, str>],
    term: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<bool> {
    Ok(find(words.len(), |index| &words[index], term, poll)?.is_some())
}

#[derive(Debug)]
pub(crate) struct PreparedSynonyms<'a> {
    entries: Vec<(Cow<'a, str>, Cow<'a, [String]>)>,
}

impl<'a> PreparedSynonyms<'a> {
    pub(super) fn borrowed(values: &'a BTreeMap<String, Vec<String>>) -> Self {
        Self {
            entries: values
                .iter()
                .map(|(key, values)| {
                    (
                        Cow::Borrowed(key.as_str()),
                        Cow::Borrowed(values.as_slice()),
                    )
                })
                .collect(),
        }
    }

    pub(super) fn owned(values: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            entries: values
                .into_iter()
                .map(|(key, values)| (Cow::Owned(key), Cow::Owned(values)))
                .collect(),
        }
    }

    pub(super) fn into_owned(self) -> PreparedSynonyms<'static> {
        PreparedSynonyms {
            entries: self
                .entries
                .into_iter()
                .map(|(key, values)| {
                    (
                        Cow::Owned(key.into_owned()),
                        Cow::Owned(values.into_owned()),
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn get(
        &self,
        term: &str,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Option<&[String]>> {
        Ok(find(
            self.entries.len(),
            |index| &self.entries[index].0,
            term,
            poll,
        )?
        .map(|index| self.entries[index].1.as_ref()))
    }
}
