//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-index changes share the scalar editor's child order and retain every constructor.

use super::{
    traversal::{Edit, EditIdentity},
    OwnedScalarExpr, ScalarExpr,
};
use crate::{catalog::work::inline, plan::ScalarInputWork, SQLError};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

pub(crate) trait QueryIndexInput<W: ScalarInputWork> {
    fn index(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<usize, SQLError>> + Send;
}

struct Immediate<'a, F: ?Sized>(&'a mut F);

impl<F: FnMut(usize) -> Result<usize, SQLError> + ?Sized> QueryIndexInput<()> for Immediate<'_, F> {
    fn index(
        &mut self,
        index: usize,
        _: &ProductionControl<'_>,
        (): &mut (),
    ) -> impl Future<Output = Result<usize, SQLError>> + Send {
        ready((self.0)(index))
    }
}

struct Indices<'a, I, const BEFORE: bool>(&'a mut I);

impl<I, const BEFORE: bool> EditIdentity for Indices<'_, I, BEFORE> {
    type Identity = ();
    const BEFORE_CHILDREN: bool = BEFORE;

    fn identity(_: &ScalarExpr) {}
}

impl<W: ScalarInputWork, I: QueryIndexInput<W>, const BEFORE: bool> Edit<W>
    for Indices<'_, I, BEFORE>
{
    fn apply(
        &mut self,
        (): (),
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<OwnedScalarExpr, SQLError>> + Send {
        let replacement = control.check().map(|()| match &expression.expression {
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => Some(self.0.index(*index, control, work)),
            _ => None,
        });
        async move {
            if let Some(replacement) = replacement? {
                let replacement = replacement.await?;
                match &mut expression.expression {
                    ScalarExpr::ScalarSubquery(index)
                    | ScalarExpr::Exists {
                        subquery: index, ..
                    }
                    | ScalarExpr::InSubquery {
                        subquery: index, ..
                    } => *index = replacement,
                    _ => unreachable!("selected scalar-query occurrence"),
                }
            }
            Ok(expression)
        }
    }
}

impl OwnedScalarExpr {
    pub(crate) fn remap_query_occurrences(
        self,
        control: &ProductionControl<'_>,
        map: &mut dyn FnMut(usize) -> Result<usize, SQLError>,
    ) -> Result<Self, SQLError> {
        inline(self.remap_query_occurrences_from(control, &mut (), &mut Immediate(map)))
    }

    pub(crate) async fn remap_query_occurrences_from<W: ScalarInputWork, I: QueryIndexInput<W>>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
        map: &mut I,
    ) -> Result<Self, SQLError>
    where
        W::Error: Into<SQLError>,
    {
        self.edit_from((), control, work, &mut Indices::<_, false>(map))
            .await
    }

    /// Remap only this expression's local scalar-query references after their query owners have moved together. Nested query arenas are not part of the scalar tree.
    pub fn remap_subquery_ids(
        self,
        control: &ProductionControl<'_>,
        map: &mut dyn FnMut(usize) -> usize,
    ) -> Result<Self, SQLError> {
        inline(self.edit_from(
            (),
            control,
            &mut (),
            &mut Indices::<_, true>(&mut Immediate(&mut |index| Ok(map(index)))),
        ))
    }
}
