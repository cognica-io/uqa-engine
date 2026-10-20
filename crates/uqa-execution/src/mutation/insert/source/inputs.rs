//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve INSERT source completion while reading only one bounded row at a time.

use std::borrow::Cow;

use uqa_core::CancellationToken;
use uqa_sql::{SQLError, ScalarExpr};

use crate::query::output::{PublicQueryRows, QueryOutput};

pub(crate) enum InsertInputRows<'a> {
    Values(std::slice::Iter<'a, Vec<ScalarExpr>>),
    Query {
        rows: Box<PublicQueryRows>,
        cancellation: CancellationToken,
    },
}

impl<'a> InsertInputRows<'a> {
    pub(crate) fn values(rows: &'a [Vec<ScalarExpr>]) -> Self {
        Self::Values(rows.iter())
    }

    pub(crate) fn query(
        output: QueryOutput,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        let rows = PublicQueryRows::new(output, "INSERT SELECT")?;
        Ok(Self::Query {
            rows: Box::new(rows),
            cancellation,
        })
    }
}

impl<'a> Iterator for InsertInputRows<'a> {
    type Item = Result<Cow<'a, [ScalarExpr]>, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Values(rows) => rows.next().map(|row| Ok(Cow::Borrowed(row.as_slice()))),
            Self::Query { rows, cancellation } => {
                if let Err(error) = cancellation.check() {
                    return Some(Err(error.into()));
                }
                rows.next().map(|row| {
                    row.map(|values| {
                        Cow::Owned(values.into_iter().map(ScalarExpr::Literal).collect())
                    })
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
