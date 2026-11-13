//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BudgetedSharedMap, Ordering};
use crate::memory::MapKeyInput;

impl<K: Ord, V> BudgetedSharedMap<K, V> {
    /// Select a retained entry through the caller's original comparison channel. Each completed comparison chooses one immutable tree edge; no synchronous comparison or allocation follows it.
    pub async fn get_from<Q: ?Sized, C: MapKeyInput<Q, K>>(
        &self,
        query: &Q,
        input: &mut C,
    ) -> Result<Option<&V>, C::Error> {
        input.checkpoint().await?;
        let mut link = &self.root;
        while let Some(node) = link {
            match input.compare(query, &node.entry.0).await? {
                Ordering::Less => link = &node.left,
                Ordering::Greater => link = &node.right,
                Ordering::Equal => return Ok(Some(&node.entry.1)),
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
