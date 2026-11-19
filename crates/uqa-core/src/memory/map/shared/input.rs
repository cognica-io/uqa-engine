//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{tree, BudgetedSharedMap, Ordering, PreparedSharedMapInsert};
use crate::memory::{MapKeyInput, MemoryError};

impl<K: Ord, V> BudgetedSharedMap<K, V> {
    /// Retain the exclusively selected path through suspended key comparisons, then admit the same structural changes as ordinary insertion. Failure or abandonment leaves the original root unchanged; publication performs no comparisons or further admission.
    pub async fn prepare_insert_from<C>(
        &mut self,
        key: K,
        input: &mut C,
    ) -> Result<PreparedSharedMapInsert<'_, K, V>, C::Error>
    where
        C: MapKeyInput<K>,
        C::Error: From<MemoryError>,
    {
        input.checkpoint().await?;
        let mut selected = tree::InsertSelection::new::<K, V>();
        let mut link = &self.root;
        while let Some(node) = link {
            let direction = input.compare(&key, &node.entry.0).await?;
            selected.observe(node, direction)?;
            link = match direction {
                Ordering::Less => &node.left,
                Ordering::Greater => &node.right,
                Ordering::Equal => break,
            };
        }
        input.checkpoint().await?;
        let insertion = selected.finish(link, key, &self.memory)?;
        Ok(PreparedSharedMapInsert {
            map: self,
            insertion,
        })
    }

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
