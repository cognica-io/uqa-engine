//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unicode source coordinates and composed character-filter edit maps.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ops::Range;

use serde::Serialize;

use crate::{AnalysisError, AnalysisResult};

mod edits;

use edits::EditMap;
pub(crate) use edits::TextEdit;

/// Half-open ranges in the same original input, expressed in both coordinate systems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceOffsets {
    pub utf8: Range<usize>,
    pub utf16: Range<usize>,
}

/// A checked conversion index containing Unicode scalar boundaries only.
#[derive(Debug, Clone)]
pub struct TextCoordinates {
    boundaries: Vec<(usize, usize)>,
    utf8_len: usize,
    utf16_len: usize,
}

impl TextCoordinates {
    pub fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Self {
                boundaries: Vec::new(),
                utf8_len: text.len(),
                utf16_len: text.len(),
            };
        }
        let mut utf16 = 0;
        let mut boundaries = Vec::new();
        for (utf8, character) in text.char_indices() {
            boundaries.push((utf8, utf16));
            utf16 += character.len_utf16();
        }
        boundaries.push((text.len(), utf16));
        Self {
            boundaries,
            utf8_len: text.len(),
            utf16_len: utf16,
        }
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
    text: Cow<'a, str>,
    maps: Vec<EditMap>,
    original_coordinates: OnceCell<TextCoordinates>,
    filtered_coordinates: OnceCell<TextCoordinates>,
}

impl<'a> FilteredText<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            original: text,
            text: Cow::Borrowed(text),
            maps: Vec::new(),
            original_coordinates: OnceCell::new(),
            filtered_coordinates: OnceCell::new(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn original(&self) -> &'a str {
        self.original
    }

    pub fn into_string(self) -> String {
        self.text.into_owned()
    }

    /// Project a filtered UTF-8 range into the covering original source range.
    pub fn source_offsets(&self, mut range: Range<usize>) -> AnalysisResult<SourceOffsets> {
        validate_utf8_range(&self.text, &range)?;
        for map in self.maps.iter().rev() {
            range = map.project(range);
        }
        self.original_coordinates().offsets(range)
    }

    /// Project filtered UTF-16 coordinates without accepting a split surrogate pair.
    pub fn source_offsets_utf16(&self, range: Range<usize>) -> AnalysisResult<SourceOffsets> {
        validate_order(&range)?;
        let coordinates = self
            .filtered_coordinates
            .get_or_init(|| TextCoordinates::new(&self.text));
        let utf8 = coordinates.utf16_to_utf8(range.start)?..coordinates.utf16_to_utf8(range.end)?;
        self.source_offsets(utf8)
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

    pub(crate) fn apply_edits(&mut self, edits: Vec<TextEdit>) -> AnalysisResult<()> {
        if let Some((text, map)) = EditMap::apply(&self.text, edits)? {
            self.text = Cow::Owned(text);
            self.maps.push(map);
            self.filtered_coordinates.take();
        }
        Ok(())
    }

    fn original_coordinates(&self) -> &TextCoordinates {
        self.original_coordinates
            .get_or_init(|| TextCoordinates::new(self.original))
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
