//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array syntax retains absent endpoints independently of nullable SQL values.

use serde::{Deserialize, Serialize};

use super::FunctionDispatch;
use crate::SQLError;

/// `PostgreSQL`'s admitted array-subscript rank, checked after input analysis.
pub const MAX_ARRAY_SUBSCRIPT_DIMENSIONS: usize = 6;

/// Only absent syntax sets a bit. A supplied NULL is an ordinary bound expression.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArraySliceBounds {
    omitted_lower: u8,
    omitted_upper: u8,
}

impl ArraySliceBounds {
    pub fn omit(&mut self, dimension: usize, lower: bool, upper: bool) {
        // An excessive rank must reach ordered type analysis before its rank error.
        if dimension < MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
            self.omitted_lower |= u8::from(lower) << dimension;
            self.omitted_upper |= u8::from(upper) << dimension;
        }
    }

    #[must_use]
    pub fn is_omitted(self, dimension: usize, lower: bool) -> bool {
        let bits = if lower {
            self.omitted_lower
        } else {
            self.omitted_upper
        };
        dimension < MAX_ARRAY_SUBSCRIPT_DIMENSIONS && bits & (1 << dimension) != 0
    }
}

/// Borrowed argument positions shared by analysis, evaluation and reconstruction.
#[derive(Debug, Clone, Copy)]
pub struct ArrayAccess {
    dimensions: usize,
    bounds: Option<ArraySliceBounds>,
}

impl ArrayAccess {
    /// Legacy slice plans use a bare NULL node for omitted syntax; typed and computed NULLs remain supplied expressions. Their original source SQL was not persisted.
    pub fn from_dispatch(
        dispatch: FunctionDispatch,
        arguments: usize,
        is_bare_null: impl Fn(usize) -> bool,
    ) -> Option<Result<Self, SQLError>> {
        let slice = match dispatch {
            FunctionDispatch::ArraySubscripts => false,
            FunctionDispatch::ArraySlices | FunctionDispatch::ArraySlicesWithBounds(_) => true,
            _ => return None,
        };
        Some((|| {
            let count = arguments
                .checked_sub(1)
                .filter(|count| *count > 0)
                .ok_or_else(|| SQLError::Internal("array access has no index".into()))?;
            if slice && !count.is_multiple_of(2) {
                return Err(SQLError::Internal(
                    "array slice has an incomplete bound pair".into(),
                ));
            }
            let dimensions = if slice { count / 2 } else { count };
            let bounds = if slice {
                let bounds = if let FunctionDispatch::ArraySlicesWithBounds(bounds) = dispatch {
                    bounds
                } else {
                    let mut bounds = ArraySliceBounds::default();
                    for dimension in 0..dimensions.min(MAX_ARRAY_SUBSCRIPT_DIMENSIONS) {
                        bounds.omit(
                            dimension,
                            is_bare_null(1 + dimension * 2),
                            is_bare_null(2 + dimension * 2),
                        );
                    }
                    bounds
                };
                Some(bounds)
            } else {
                None
            };
            Ok(Self { dimensions, bounds })
        })())
    }

    pub fn validate_dimensions(self) -> Result<(), SQLError> {
        if self.dimensions > MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
            return Err(SQLError::Routine {
                sqlstate: "54000".into(),
                message: format!("number of array dimensions ({}) exceeds the maximum allowed ({MAX_ARRAY_SUBSCRIPT_DIMENSIONS})", self.dimensions),
            });
        }
        Ok(())
    }

    #[must_use]
    pub const fn slice_bounds(self) -> Option<ArraySliceBounds> {
        self.bounds
    }

    #[must_use]
    pub const fn bound_count(self) -> usize {
        self.dimensions * if self.bounds.is_some() { 2 } else { 1 }
    }

    #[must_use]
    pub fn is_omitted(self, argument: usize) -> bool {
        argument > 0
            && self
                .bounds
                .is_some_and(|bounds| bounds.is_omitted((argument - 1) / 2, argument % 2 == 1))
    }

    /// Execution visits all upper bounds, then all lower bounds. Analysis retains written lower/upper order instead.
    pub fn evaluation_order(self) -> impl Iterator<Item = usize> {
        (0..self.bound_count())
            .map(move |position| {
                if self.bounds.is_none() {
                    position + 1
                } else if position < self.dimensions {
                    position * 2 + 2
                } else {
                    (position - self.dimensions) * 2 + 1
                }
            })
            .filter(move |position| !self.is_omitted(*position))
    }
}
