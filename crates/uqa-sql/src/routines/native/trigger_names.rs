//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    catalog::work::{reference, ReferenceWork, Work},
    SQLError,
};
use std::cmp::Ordering;
use uqa_core::{memory::MapKeyInput, RelationName};

#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Name<'a> {
    schema: RelationName<'a>,
    function: RelationName<'a>,
}

pub(super) async fn name<'a, W: Work + Send>(
    source: &'a str,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<Name<'a>, SQLError> {
    let parsed = reference(source, input.control, input.work)
        .await?
        .ok_or_else(|| super::invalid("invalid trigger function name"))?;
    Ok(Name {
        schema: parsed
            .schema
            .ok_or_else(|| super::invalid("unqualified trigger function name"))?,
        function: parsed.name,
    })
}

/// Borrow the same decoded identifier characters as `RelationName::cmp`. Quoted spellings and escaped quotes keep their original ordering across work windows.
async fn compare<W: Work + Send>(
    left: RelationName<'_>,
    right: RelationName<'_>,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<Ordering, SQLError> {
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        input.checkpoint().await?;
        let order = left.by_ref().take(1024).cmp(right.by_ref().take(1024));
        if order.is_ne() {
            return Ok(order);
        }
        let (left_remains, right_remains) = (left.peek().is_some(), right.peek().is_some());
        if !left_remains || !right_remains {
            return Ok(left_remains.cmp(&right_remains));
        }
    }
}

impl<'q, 's, W: Work + Send> MapKeyInput<Name<'q>, Name<'s>> for ReferenceWork<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Self::checkpoint(self).await
    }
    async fn compare(
        &mut self,
        query: &Name<'q>,
        stored: &Name<'s>,
    ) -> Result<Ordering, Self::Error> {
        let schema = compare(query.schema, stored.schema, self).await?;
        if schema.is_eq() {
            compare(query.function, stored.function, self).await
        } else {
            Ok(schema)
        }
    }
}
