//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unknown outer fields are buffered in input order before a flattened declaration is interpreted.

use super::{
    Input, JsonDocument, JsonReadError, JsonReadWork, JsonValue, Native, Projected, Result, Work,
};

struct Field<'a> {
    source: JsonValue<'a>,
    prepared: Option<JsonDocument>,
}

pub(in crate::ast::query_input) struct FlatFields<'a, const N: usize> {
    names: &'static [&'static str; N],
    fields: [Option<Field<'a>>; N],
    order: [usize; N],
    length: usize,
}

impl<'a, const N: usize> FlatFields<'a, N> {
    pub(in crate::ast::query_input) fn new(names: &'static [&'static str; N]) -> Self {
        Self {
            names,
            fields: [const { None }; N],
            order: [0; N],
            length: 0,
        }
    }

    pub(in crate::ast::query_input) async fn observe(
        &mut self,
        input: Input<'a>,
        name: &str,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<()> {
        let prepared = input.buffer(work).await?;
        if let Some(index) = self.names.iter().position(|expected| *expected == name) {
            debug_assert!(self.fields[index].is_none(), "generic map names are unique");
            self.fields[index] = Some(Field {
                source: input.value,
                prepared,
            });
            self.order[self.length] = index;
            self.length += 1;
        }
        Ok(())
    }

    pub(super) fn values<'b>(
        &'b self,
        input: Input<'a>,
    ) -> impl Iterator<Item = (usize, FlatInput<'a, 'b>)> + 'b
    where
        'a: 'b,
    {
        self.order[..self.length].iter().map(move |&index| {
            let field = self.fields[index]
                .as_ref()
                .expect("observed flattened field");
            (
                index,
                FlatInput {
                    input: input.child(field.source),
                    prepared: field.prepared.as_ref(),
                },
            )
        })
    }
}

pub(super) struct FlatInput<'a, 'b> {
    input: Input<'a>,
    prepared: Option<&'b JsonDocument>,
}

impl<'a> FlatInput<'a, '_> {
    pub(super) async fn read<T: Projected>(self, work: &mut Work<'a>) -> Result<Native<T>> {
        if self.prepared.is_some() {
            self.input
                .buffered_value(self.prepared)
                .run::<T, _>(work)?
                .await
        } else {
            Input {
                buffered: true,
                ..self.input
            }
            .read(work)
            .await
        }
    }
}
