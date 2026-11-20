//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::work::Work;
use crate::{
    schema::{production::indexes::Indexes, ColumnIdentity, SchemaMap, SchemaSet},
    SQLError,
};
use uqa_core::memory::{OwnedMap, Produced};

#[derive(Default)]
pub(super) struct Names {
    exact: OwnedMap<Box<str>, Produced<usize>>,
    qualified: OwnedMap<ColumnIdentity, Produced<usize>>,
    ambiguous: OwnedMap<Box<str>, Produced<()>>,
    ambiguous_qualified: OwnedMap<ColumnIdentity, Produced<()>>,
}

impl Names {
    pub async fn insert<W: crate::catalog::CatalogWork + Send>(
        &mut self,
        qualifier: &str,
        name: &str,
        position: usize,
        work: &mut Work<'_, '_, W>,
    ) -> Result<(), SQLError> {
        let control = work.control;
        let key = work.boxed_text(name).await?;
        let duplicate = work
            .insert(
                &mut self.exact,
                key,
                control.finish(position, control.empty_reservation())?,
            )
            .await?;
        let key = work.identity(qualifier, name).await?;
        let qualified_duplicate = work
            .insert(
                &mut self.qualified,
                key,
                control.finish(position, control.empty_reservation())?,
            )
            .await?;
        if duplicate {
            let key = work.boxed_text(name).await?;
            work.insert(
                &mut self.ambiguous,
                key,
                control.finish((), control.empty_reservation())?,
            )
            .await?;
        }
        if qualified_duplicate {
            let key = work.identity(qualifier, name).await?;
            work.insert(
                &mut self.ambiguous_qualified,
                key,
                control.finish((), control.empty_reservation())?,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn finish<W: crate::catalog::CatalogWork + Send>(
        self,
        work: &mut Work<'_, '_, W>,
    ) -> Result<Indexes, SQLError> {
        // A visible column’s display name is its unqualified name. Both indexes select its last position.
        let exact = SchemaMap::from_ordered(self.exact, &work.control, work.input).await?;
        Ok(Indexes {
            unqualified: exact.clone(),
            exact,
            qualified: SchemaMap::from_ordered(self.qualified, &work.control, work.input).await?,
            ambiguous_unqualified: SchemaSet::from_map(
                SchemaMap::from_ordered(self.ambiguous, &work.control, work.input).await?,
            ),
            ambiguous_qualified: SchemaSet::from_map(
                SchemaMap::from_ordered(self.ambiguous_qualified, &work.control, work.input)
                    .await?,
            ),
        })
    }
}
