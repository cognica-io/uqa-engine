//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unicode source coordinates and composed character-filter edit maps.

use std::cell::OnceCell;
use std::ops::Range;
use std::sync::Arc;

use serde::Serialize;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use crate::{AnalysisError, AnalysisResult};

mod edits;
mod maps;
mod runtime;

pub(crate) use edits::{EditBuilder, EditedText};
use maps::EditMaps;
use runtime::SourceText;

/// Half-open source ranges: exact UTF-16 units and the covering UTF-8 scalar range.
///
/// Strict projection methods require both ranges to address identical scalar boundaries. Explicit covering methods retain UTF-16 boundaries inside surrogate pairs while covering each touched scalar in UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceOffsets {
    pub utf8: Range<usize>,
    pub utf16: Range<usize>,
}

/// A checked conversion index containing Unicode scalar boundaries only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCoordinates {
    boundaries: Vec<(usize, usize)>,
    utf8_len: usize,
    utf16_len: usize,
}

impl TextCoordinates {
    pub fn new(text: &str) -> Self {
        Self::new_budgeted(text, &MemoryBudget::new(usize::MAX), &mut || Ok(()))
            .expect("unbounded source coordinate allocation")
            .into_parts()
            .0
    }

    /// Construct a scalar index with caller-owned byte limits and cancellation. ASCII needs no boundary buffer; other input reserves one entry per scalar and its end boundary.
    pub fn new_budgeted(
        text: &str,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let mut ascii = true;
        for chunk in text.as_bytes().chunks(1024) {
            poll()?;
            if !chunk.is_ascii() {
                ascii = false;
                break;
            }
        }
        let mut boundaries = BudgetedVec::new(budget);
        if ascii {
            return Ok(Budgeted::new(
                Self {
                    boundaries: Vec::new(),
                    utf8_len: text.len(),
                    utf16_len: text.len(),
                },
                budget.empty_reservation(),
            ));
        }
        let mut scalars = 0;
        for _ in text.chars() {
            if scalars % 1024 == 0 {
                poll()?;
            }
            scalars += 1;
        }
        boundaries.reserve(scalars + 1)?;
        let mut utf16 = 0;
        for (index, (utf8, character)) in text.char_indices().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            boundaries.push((utf8, utf16))?;
            utf16 += character.len_utf16();
        }
        boundaries.push((text.len(), utf16))?;
        poll()?;
        let (boundaries, memory) = boundaries.into_parts();
        Ok(Budgeted::new(
            Self {
                boundaries,
                utf8_len: text.len(),
                utf16_len: utf16,
            },
            memory,
        ))
    }

    pub fn utf8_len(&self) -> usize {
        self.utf8_len
    }

    pub fn utf16_len(&self) -> usize {
        self.utf16_len
    }

    pub fn utf8_to_utf16(&self, offset: usize) -> AnalysisResult<usize> {
        if self.boundaries.is_empty() && offset <= self.utf8_len {
            return Ok(offset);
        }
        self.boundaries
            .binary_search_by_key(&offset, |point| point.0)
            .map(|index| self.boundaries[index].1)
            .map_err(|_| AnalysisError::InvalidTextOffset {
                coordinate: "UTF-8",
                offset,
                length: self.utf8_len(),
            })
    }

    pub fn utf16_to_utf8(&self, offset: usize) -> AnalysisResult<usize> {
        if self.boundaries.is_empty() && offset <= self.utf16_len {
            return Ok(offset);
        }
        self.boundaries
            .binary_search_by_key(&offset, |point| point.1)
            .map(|index| self.boundaries[index].0)
            .map_err(|_| AnalysisError::InvalidTextOffset {
                coordinate: "UTF-16",
                offset,
                length: self.utf16_len(),
            })
    }

    pub fn offsets(&self, utf8: Range<usize>) -> AnalysisResult<SourceOffsets> {
        validate_order(&utf8)?;
        let utf16 = self.utf8_to_utf16(utf8.start)?..self.utf8_to_utf16(utf8.end)?;
        Ok(SourceOffsets { utf8, utf16 })
    }

    /// Retain exact UTF-16 coordinates and cover split surrogate pairs in UTF-8.
    ///
    /// An empty range inside a pair covers that scalar; an empty range at a scalar boundary remains empty.
    pub fn covering_offsets_utf16(&self, utf16: Range<usize>) -> AnalysisResult<SourceOffsets> {
        self.validate_utf16_range(&utf16)?;
        if self.boundaries.is_empty() {
            return Ok(SourceOffsets {
                utf8: utf16.clone(),
                utf16,
            });
        }
        let start = match self
            .boundaries
            .binary_search_by_key(&utf16.start, |point| point.1)
        {
            Ok(index) => self.boundaries[index].0,
            Err(index) => self.boundaries[index - 1].0,
        };
        let end = match self
            .boundaries
            .binary_search_by_key(&utf16.end, |point| point.1)
        {
            Ok(index) | Err(index) => self.boundaries[index].0,
        };
        Ok(SourceOffsets {
            utf8: start..end,
            utf16,
        })
    }

    fn validate_utf16_range(&self, range: &Range<usize>) -> AnalysisResult<()> {
        validate_order(range)?;
        if range.end > self.utf16_len {
            return Err(AnalysisError::InvalidTextOffset {
                coordinate: "UTF-16",
                offset: range.end,
                length: self.utf16_len,
            });
        }
        Ok(())
    }
}

/// Filtered text retaining its original input and the provenance of every edit.
///
/// Replacement output covers the replaced source range. Insertions map to an empty source range. An empty output range uses the following source boundary, including trailing deletions at the end of the input.
///
/// ```
/// use uqa_analysis::CharFilter;
/// let input = "<b>한&amp;🙂</b>";
/// let filtered = CharFilter::HTMLStrip.filter_with_offsets(input)?;
/// assert_eq!(filtered.as_str(), " 한&🙂 ");
/// let entity = filtered.source_offsets(4..5)?;
/// assert_eq!(&input[entity.utf8], "&amp;");
/// assert_eq!(entity.utf16, 4..9);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
#[derive(Debug, Clone)]
pub struct FilteredText<'a> {
    original: &'a str,
    text: SourceText<'a>,
    maps: Option<Arc<EditMaps>>,
    original_coordinates: OnceCell<Arc<Budgeted<TextCoordinates>>>,
    filtered_coordinates: OnceCell<Arc<Budgeted<TextCoordinates>>>,
}

impl<'a> FilteredText<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            original: text,
            text: SourceText::Borrowed(text),
            maps: None,
            original_coordinates: OnceCell::new(),
            filtered_coordinates: OnceCell::new(),
        }
    }

    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }

    pub fn original(&self) -> &'a str {
        self.original
    }

    pub fn into_string(self) -> String {
        self.text.into_string()
    }

    pub(crate) fn unbounded_budget(&self) -> MemoryBudget {
        self.maps
            .as_ref()
            .map(|maps| maps.budget())
            .filter(|budget| budget.limit() == usize::MAX)
            .cloned()
            .unwrap_or_else(|| MemoryBudget::new(usize::MAX))
    }

    /// Project a filtered UTF-8 range into the covering original source range.
    pub fn source_offsets(&self, mut range: Range<usize>) -> AnalysisResult<SourceOffsets> {
        validate_utf8_range(self.as_str(), &range)?;
        if let Some(maps) = &self.maps {
            for map in maps.iter().rev() {
                range = map.project(range);
            }
        }
        self.original_coordinates().offsets(range)
    }

    /// Project filtered UTF-16 coordinates without accepting a split surrogate pair.
    pub fn source_offsets_utf16(&self, range: Range<usize>) -> AnalysisResult<SourceOffsets> {
        validate_order(&range)?;
        let coordinates = self.filtered_coordinates();
        let utf8 = coordinates.utf16_to_utf8(range.start)?..coordinates.utf16_to_utf8(range.end)?;
        self.source_offsets(utf8)
    }

    /// Project exact filtered UTF-16 units, preserving split pairs through every edit map.
    ///
    /// The returned original UTF-16 range stays exact; UTF-8 covers the original scalars. Replacements cover their source and insertions retain their source boundary, as in strict projection.
    ///
    /// ```
    /// use uqa_analysis::CharFilter;
    /// let filtered = CharFilter::HTMLStrip.filter_with_offsets("<b>🙂a</b>")?;
    /// let source = filtered.source_covering_offsets_utf16(2..3)?;
    /// assert_eq!(source.utf16, 4..5);
    /// assert_eq!(source.utf8, 3..7);
    /// assert_eq!(&filtered.original()[source.utf8], "🙂");
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn source_covering_offsets_utf16(
        &self,
        mut range: Range<usize>,
    ) -> AnalysisResult<SourceOffsets> {
        self.filtered_coordinates().validate_utf16_range(&range)?;
        if let Some(maps) = &self.maps {
            for map in maps.iter().rev() {
                range = map.project_utf16(range);
            }
        }
        self.original_coordinates().covering_offsets_utf16(range)
    }

    /// The final input boundary, even when filters remove all source characters.
    pub fn final_offsets(&self) -> SourceOffsets {
        let utf8 = self.original.len();
        let utf16 = self.original_coordinates().utf16_len();
        SourceOffsets {
            utf8: utf8..utf8,
            utf16: utf16..utf16,
        }
    }

    pub(crate) fn apply_edited(
        &mut self,
        edited: Option<EditedText>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let Some(edited) = edited else {
            return Ok(());
        };
        poll()?;
        let text = edited.text.into_shared()?;
        let map = edited.map.into_shared()?;
        EditMaps::push(&mut self.maps, map, budget, poll)?;
        self.text = SourceText::Owned(text);
        self.filtered_coordinates.take();
        Ok(())
    }

    pub(crate) fn prepare_coordinates(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        poll()?;
        if self.original_coordinates.get().is_none() {
            let coordinates =
                TextCoordinates::new_budgeted(self.original, budget, poll)?.into_shared()?;
            self.original_coordinates
                .set(coordinates)
                .expect("uninitialized original coordinates");
        }
        if self.maps.is_some() && self.filtered_coordinates.get().is_none() {
            let coordinates =
                TextCoordinates::new_budgeted(self.as_str(), budget, poll)?.into_shared()?;
            self.filtered_coordinates
                .set(coordinates)
                .expect("uninitialized filtered coordinates");
        }
        Ok(())
    }

    pub(crate) fn filtered_utf16(&self, range: Range<usize>) -> AnalysisResult<Range<usize>> {
        Ok(self.filtered_coordinates().offsets(range)?.utf16)
    }

    #[cfg(feature = "nori")]
    pub(crate) fn projection(&self) -> Arc<Budgeted<SourceProjection>> {
        let budget = MemoryBudget::new(usize::MAX);
        self.projection_budgeted(&budget, &mut || Ok(()))
            .expect("unbounded source retention")
    }

    #[cfg(feature = "nori")]
    pub(crate) fn projection_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Arc<Budgeted<SourceProjection>>> {
        self.prepare_coordinates(budget, poll)?;
        let source = crate::allocation::copy_text(self.original, budget, poll)?.into_shared()?;
        let projection = SourceProjection {
            source,
            maps: self.maps.clone(),
            original: self.original_coordinates().clone(),
            filtered: self.filtered_coordinates().clone(),
        };
        Ok(Budgeted::new(projection, budget.empty_reservation()).into_shared()?)
    }

    fn original_coordinates(&self) -> &Arc<Budgeted<TextCoordinates>> {
        self.original_coordinates.get_or_init(|| {
            let budget = MemoryBudget::new(usize::MAX);
            TextCoordinates::new_budgeted(self.original, &budget, &mut || Ok(()))
                .and_then(|coordinates| Ok(coordinates.into_shared()?))
                .expect("unbounded original source coordinates")
        })
    }

    fn filtered_coordinates(&self) -> &Arc<Budgeted<TextCoordinates>> {
        if self.maps.is_none() {
            return self.original_coordinates();
        }
        self.filtered_coordinates.get_or_init(|| {
            let budget = MemoryBudget::new(usize::MAX);
            TextCoordinates::new_budgeted(self.as_str(), &budget, &mut || Ok(()))
                .and_then(|coordinates| Ok(coordinates.into_shared()?))
                .expect("unbounded filtered source coordinates")
        })
    }
}

/// Shared source provenance for tokens composed after the original input borrow ends.
#[cfg(feature = "nori")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProjection {
    source: Arc<Budgeted<String>>,
    maps: Option<Arc<EditMaps>>,
    original: Arc<Budgeted<TextCoordinates>>,
    filtered: Arc<Budgeted<TextCoordinates>>,
}

#[cfg(feature = "nori")]
impl SourceProjection {
    pub fn filtered_len(&self) -> usize {
        self.filtered.utf16_len()
    }

    pub(crate) fn project_with_control(
        &self,
        mut range: Range<usize>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<SourceOffsets> {
        poll()?;
        self.filtered.validate_utf16_range(&range)?;
        if let Some(maps) = &self.maps {
            for map in maps.iter().rev() {
                poll()?;
                range = map.project_utf16(range);
            }
        }
        self.original.covering_offsets_utf16(range)
    }

    pub(crate) fn is_verbatim_with_control(
        &self,
        term: &crate::TokenTerm,
        offsets: &SourceOffsets,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<bool> {
        poll()?;
        let Some(text) = term.as_str() else {
            return Ok(false);
        };
        let Some(source) = self.source.get(offsets.utf8.clone()) else {
            return Ok(false);
        };
        if text.len() != source.len() {
            return Ok(false);
        }
        for (text, source) in text
            .as_bytes()
            .chunks(1024)
            .zip(source.as_bytes().chunks(1024))
        {
            poll()?;
            if text != source {
                return Ok(false);
            }
        }
        let mut length = 0;
        for (index, unit) in text.chars().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            length += unit.len_utf16();
        }
        Ok(length == offsets.utf16.len())
    }
}

fn validate_order(range: &Range<usize>) -> AnalysisResult<()> {
    if range.start > range.end {
        return Err(AnalysisError::InvalidTextSpan {
            start: range.start,
            end: range.end,
        });
    }
    Ok(())
}

fn validate_utf8_range(text: &str, range: &Range<usize>) -> AnalysisResult<()> {
    validate_order(range)?;
    for offset in [range.start, range.end] {
        if !text.is_char_boundary(offset) {
            return Err(AnalysisError::InvalidTextOffset {
                coordinate: "UTF-8",
                offset,
                length: text.len(),
            });
        }
    }
    Ok(())
}
