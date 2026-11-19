//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected membership lookup borrows decoded relation components without a second identity allocation.

use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, OwnedMap, ProductionControl},
    RelationIdentity, RelationName, RelationReference,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// Find a fully qualified GIN table in its caller-selected immutable membership generation.
pub async fn lookup_table_from<'a, T: Sync>(
    tables: &'a OwnedMap<RelationIdentity, T>,
    name: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<&'a T>> {
    let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        match control.format(format_args!("{error}")) {
            Ok(message) => super::corrupt(message.into_parts().0),
            Err(error) => error.into(),
        }
    })?;
    tables.get_from(&reference, &mut Lookup(input)).await
}

struct Lookup<'a, 'c>(&'a mut AsyncImageInput<'c>);
impl<'q> MapKeyInput<RelationReference<'q>, RelationIdentity> for Lookup<'_, '_> {
    type Error = StorageBackendError;
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }
    async fn compare(
        &mut self,
        query: &RelationReference<'q>,
        stored: &RelationIdentity,
    ) -> StorageBackendResult<Ordering> {
        let schema = compare(
            query.schema.unwrap_or(RelationName::decoded("public")),
            &stored.schema,
            self.0,
        )
        .await?;
        if schema != Ordering::Equal {
            return Ok(schema);
        }
        compare(query.name, &stored.name, self.0).await
    }
}
async fn compare(
    left: RelationName<'_>,
    right: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Ordering> {
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        input.checkpoint().await?;
        for _ in 0..1024 {
            match (left.next(), right.next()) {
                (Some(left), Some(right)) if left == right => (),
                (left, right) => return Ok(left.cmp(&right)),
            }
        }
    }
}
