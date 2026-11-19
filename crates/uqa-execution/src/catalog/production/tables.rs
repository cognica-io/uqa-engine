//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog table membership adopts admitted names and shares immutable metadata generations.

use super::super::{CatalogReadSnapshot, CatalogTableSnapshot};
use std::{cmp::Ordering, future::Future};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, MemoryBudget, OwnedMap, Produced, ProductionControl},
    CancellationToken, RelationIdentity, RelationName, RelationReference,
};
use uqa_sql::SQLError;

type Tables = OwnedMap<RelationIdentity, CatalogTableSnapshot>;

impl CatalogReadSnapshot {
    /// Copy selected membership identities while sharing each table's original metadata owners. The caller owns selection and synchronization of its table generation.
    pub fn capture_tables_with_control<'a>(
        tables: impl IntoIterator<Item = (&'a RelationIdentity, CatalogTableSnapshot)>,
        control: &ProductionControl<'_>,
    ) -> Result<Tables, SQLError> {
        crate::catalog::selection_work::inline(capture(tables, control, &mut ()))
    }

    /// Resume the same membership capture through admitted identity construction and bounded comparisons. No source lock or Engine reference belongs to the output.
    pub async fn capture_tables_from_input<'a>(
        tables: impl IntoIterator<Item = (&'a RelationIdentity, CatalogTableSnapshot)>,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Tables, SQLError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        capture(tables, &control, input).await
    }
}

async fn capture<'a>(
    source: impl IntoIterator<Item = (&'a RelationIdentity, CatalogTableSnapshot)>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Tables, SQLError> {
    control.check()?;
    let mut tables = Tables::new();
    for (relation, table) in source {
        let reference = RelationReference {
            schema: Some(RelationName::decoded(&relation.schema)),
            name: RelationName::decoded(&relation.name),
        };
        let relation = work.identity(reference, control).await?;
        match relation.into_budgeted() {
            Ok(relation) => {
                tables
                    .insert_admitted_key_from(relation, table, &mut Comparison { control, work })
                    .await?;
            }
            Err(relation) => {
                let relation = relation
                    .into_uncontrolled()
                    .unwrap_or_else(|_| unreachable!("ordinary identity"));
                tables.insert(relation, table);
            }
        }
    }
    control.check()?;
    Ok(tables)
}

trait Work: crate::catalog::selection_work::Work {
    fn identity(
        &mut self,
        reference: RelationReference<'_>,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<RelationIdentity>, SQLError>> + Send;
}

impl Work for () {
    async fn identity(
        &mut self,
        reference: RelationReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationIdentity>, SQLError> {
        Ok(reference.identity_with_control(RelationName::decoded("public"), control)?)
    }
}

impl Work for AsyncImageInput<'_> {
    async fn identity(
        &mut self,
        reference: RelationReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationIdentity>, SQLError> {
        Ok(reference
            .identity_from_input(RelationName::decoded("public"), control, self)
            .await?)
    }
}

struct Comparison<'a, 'c, W> {
    control: &'a ProductionControl<'c>,
    work: &'a mut W,
}

impl<W: Work> MapKeyInput<RelationIdentity> for Comparison<'_, '_, W> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        crate::catalog::selection_work::checkpoint(self.control, self.work).await
    }
    async fn compare(
        &mut self,
        left: &RelationIdentity,
        right: &RelationIdentity,
    ) -> Result<Ordering, SQLError> {
        let schema = self.text(&left.schema, &right.schema).await?;
        if schema != Ordering::Equal {
            return Ok(schema);
        }
        self.text(&left.name, &right.name).await
    }
}

impl<W: Work> Comparison<'_, '_, W> {
    async fn text(&mut self, left: &str, right: &str) -> Result<Ordering, SQLError> {
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let order = left.cmp(right);
            if order != Ordering::Equal {
                return Ok(order);
            }
        }
        Ok(left.len().cmp(&right.len()))
    }
}

#[cfg(test)]
mod tests;
