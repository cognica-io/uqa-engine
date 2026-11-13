//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Suspended comparisons retain an exclusive insertion path before any publication.

use super::{tree, Node, OwnedMap, OwnedNode, MAX_HEIGHT};
use crate::memory::{Budgeted, MemoryError};
use std::{cmp::Ordering, future::Future};

/// Compare a borrowed query with an original stored key through the caller's work channel. Ordering must agree with the map's ordinary key order. The comparison owns its cancellation and work checks; no synchronous key comparison follows it.
pub trait MapKeyInput<Q: ?Sized, K = Q> {
    type Error;

    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn compare(
        &mut self,
        query: &Q,
        stored: &K,
    ) -> impl Future<Output = Result<Ordering, Self::Error>> + Send;
}

impl<K: Ord, V> OwnedMap<K, V> {
    /// Borrow the selected value without constructing a query key. Each tree edge is selected by one completed comparison; suspension or failure leaves the map unchanged.
    pub async fn get_from<Q: ?Sized, C: MapKeyInput<Q, K>>(
        &self,
        query: &Q,
        input: &mut C,
    ) -> Result<Option<&V>, C::Error> {
        input.checkpoint().await?;
        let mut link = &self.root;
        while let Some(node) = link {
            match input.compare(query, &node.key).await? {
                Ordering::Less => link = &node.left,
                Ordering::Greater => link = &node.right,
                Ordering::Equal => return Ok(Some(&node.value.value)),
            }
        }
        Ok(None)
    }

    /// Adopt one admitted key after all comparisons complete. The exclusive borrow keeps the selected path valid across suspension. Replacement preserves the original key and node; a new node is admitted before allocation and then published without further comparison or fallible work.
    pub async fn insert_admitted_key_from<C>(
        &mut self,
        key: Budgeted<K>,
        value: V,
        input: &mut C,
    ) -> Result<Option<V>, C::Error>
    where
        C: MapKeyInput<K>,
        C::Error: From<MemoryError>,
    {
        input.checkpoint().await?;
        let mut path = [Ordering::Less; MAX_HEIGHT];
        let mut depth = 0;
        let mut link = &mut self.root;
        while let Some(node) = link {
            let direction = input.compare(&key, &node.key).await?;
            let node = &mut *node.value;
            match direction {
                Ordering::Less => link = &mut node.left,
                Ordering::Greater => link = &mut node.right,
                Ordering::Equal => {
                    input.checkpoint().await?;
                    return Ok(Some(std::mem::replace(&mut node.value, value)));
                }
            }
            path[depth] = direction;
            depth += 1;
        }
        input.checkpoint().await?;
        let mut memory = key.budget().reserve(Self::entry_bytes())?;
        let (key, key_memory) = key.into_parts();
        memory.absorb(key_memory);
        let entry = OwnedNode {
            value: Box::new(Node {
                key,
                value,
                left: None,
                right: None,
                height: 1,
            }),
            memory: Some(memory),
        };
        tree::insert_selected(&mut self.root, entry, &path[..depth]);
        self.len += 1;
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
