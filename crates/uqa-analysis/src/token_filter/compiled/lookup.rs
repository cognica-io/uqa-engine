//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared ordered lookup tables permit cancellation within long key comparisons.

use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use crate::AnalysisResult;

#[cfg(test)]
mod tests;

pub(super) async fn stop_words<'a>(
    language: &str,
    custom_words: &'a [String],
    work: &mut impl crate::work::Work<crate::AnalysisError>,
) -> AnalysisResult<Vec<Cow<'a, str>>> {
    let mut words = BTreeSet::new();
    for (index, word) in super::super::builtin_stop_words(language)
        .iter()
        .copied()
        .chain(custom_words.iter().map(String::as_str))
        .enumerate()
    {
        work.records(index).await?;
        words.insert(Cow::Borrowed(word));
    }
    let mut ordered = Vec::with_capacity(words.len());
    for (index, word) in words.into_iter().enumerate() {
        work.records(index).await?;
        ordered.push(word);
    }
    Ok(ordered)
}

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
    #[cfg(test)]
    pub(super) fn borrowed(values: &'a BTreeMap<String, Vec<String>>) -> Self {
        crate::work::inline(Self::borrowed_with(values, &mut ()))
            .unwrap_or_else(|never: std::convert::Infallible| match never {})
    }

    pub(super) async fn borrowed_with<E>(
        values: &'a BTreeMap<String, Vec<String>>,
        work: &mut impl crate::work::Work<E>,
    ) -> Result<Self, E> {
        let mut entries = Vec::with_capacity(values.len());
        for (index, (key, values)) in values.iter().enumerate() {
            work.records(index).await?;
            entries.push((
                Cow::Borrowed(key.as_str()),
                Cow::Borrowed(values.as_slice()),
            ));
        }
        Ok(Self { entries })
    }

    pub(super) fn owned(values: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            entries: values
                .into_iter()
                .map(|(key, values)| (Cow::Owned(key), Cow::Owned(values)))
                .collect(),
        }
    }

    #[cfg(test)]
    pub(super) fn into_owned(self) -> PreparedSynonyms<'static> {
        crate::work::inline(self.into_owned_with(&mut ()))
            .unwrap_or_else(|never: std::convert::Infallible| match never {})
    }

    pub(super) async fn into_owned_with<E>(
        self,
        work: &mut impl crate::work::Work<E>,
    ) -> Result<PreparedSynonyms<'static>, E> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for (key, values) in self.entries {
            let key = crate::work::own_text(key, work).await?;
            let values = match values {
                Cow::Owned(values) => values,
                Cow::Borrowed(values) => {
                    let mut owned = Vec::with_capacity(values.len());
                    for value in values {
                        owned.push(crate::work::copy_text(value, work).await?);
                    }
                    owned
                }
            };
            entries.push((Cow::Owned(key), Cow::Owned(values)));
        }
        Ok(PreparedSynonyms { entries })
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
