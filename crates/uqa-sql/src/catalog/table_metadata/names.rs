//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A new physical field detaches its metadata generation through admitted native copies.

use super::{SharedTableMetadata, TableMetadata};
use crate::SQLError;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryError},
};

impl SharedTableMetadata<Vec<String>> {
    /// Prepare one successor without publishing it or modifying the selected generation. The caller publishes the completed owner at its original per-field boundary; every old reader retains the preceding names and their own allowance.
    pub async fn with_appended_name_from(
        &self,
        name: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<TableMetadata<Vec<String>>, SQLError> {
        input.checkpoint().await?;
        let mut names = BudgetedVec::new(input.memory);
        names.reserve(self.len().checked_add(1).ok_or(MemoryError::SizeOverflow)?)?;
        let mut memory = input.memory.empty_reservation();
        for name in self.iter().map(String::as_str).chain(std::iter::once(name)) {
            input.checkpoint().await?;
            let (name, name_memory) = input.copy_text(name).await?.into_parts();
            memory.absorb(name_memory);
            names.push(name)?;
        }
        let (names, vector_memory) = names.into_parts();
        memory.absorb(vector_memory);
        TableMetadata::from_decoded(Budgeted::new(names, memory), input.cancellation)
    }
}
