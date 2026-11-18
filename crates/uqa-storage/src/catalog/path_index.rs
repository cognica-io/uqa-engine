//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical identities for graph-scoped path indexes, including preexisting flattened definitions.

use crate::{
    image_io::work::{inline, Work},
    StorageBackendError, StorageBackendResult,
};
use std::{borrow::Cow, future::Future};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, MemoryBudget},
};

mod codec;
pub use codec::{key, prefixes};

const WINDOW: usize = 256;

#[derive(Clone)]
struct Separators<'a> {
    raw: &'a str,
    offset: usize,
}

impl<'a> Iterator for Separators<'a> {
    type Item = Option<(&'a str, &'a str)>;

    fn next(&mut self) -> Option<Self::Item> {
        let last = self.raw.len().saturating_sub(1);
        if self.offset >= last {
            return None;
        }
        let end = self.offset.saturating_add(WINDOW).min(last);
        let bytes = &self.raw.as_bytes()[self.offset..=end];
        if let Some(relative) = bytes.windows(2).position(|s| s == b"::") {
            let cut = self.offset + relative;
            self.offset = cut + 1;
            Some(Some((&self.raw[..cut], &self.raw[cut + 2..])))
        } else {
            self.offset = end;
            Some(None)
        }
    }
}

/// All component boundaries represented by a legacy key, including overlapping delimiters.
pub fn legacy_candidates(raw: &str) -> impl Iterator<Item = (&str, &str)> + Clone {
    Separators { raw, offset: 0 }.flatten()
}

enum Component<'a> {
    Borrowed(&'a str),
    Decoded(Budgeted<String>),
}

impl AsRef<str> for Component<'_> {
    fn as_ref(&self) -> &str {
        match self {
            Self::Borrowed(value) => value,
            Self::Decoded(value) => value.as_str(),
        }
    }
}

impl<'a> Component<'a> {
    fn into_cow(self) -> Cow<'a, str> {
        match self {
            Self::Borrowed(value) => Cow::Borrowed(value),
            Self::Decoded(value) => Cow::Owned(value.into_parts().0),
        }
    }
}

struct Identity<'a> {
    graph: Component<'a>,
    name: Component<'a>,
    canonical: bool,
}

impl<'a> Identity<'a> {
    fn raw((graph, name): (&'a str, &'a str), canonical: bool) -> Self {
        Self {
            graph: Component::Borrowed(graph),
            name: Component::Borrowed(name),
            canonical,
        }
    }

    fn parts(self) -> (Cow<'a, str>, Cow<'a, str>) {
        (self.graph.into_cow(), self.name.into_cow())
    }
}

/// Look up a graph on the same captured catalog as the path-index definition.
pub trait PathIndexGraphs: Sync {
    fn contains_from(
        &self,
        graph: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = StorageBackendResult<bool>> + Send;
}

trait Graphs: Work {
    fn contains(&mut self, graph: &str) -> impl Future<Output = StorageBackendResult<bool>> + Send;
}

struct Catalog<F>(F);

impl<F> Work for Catalog<F> {
    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send {
        std::future::ready(Ok(()))
    }
}

impl<F: FnMut(&str) -> StorageBackendResult<bool>> Graphs for Catalog<F> {
    fn contains(&mut self, graph: &str) -> impl Future<Output = StorageBackendResult<bool>> + Send {
        std::future::ready((self.0)(graph))
    }
}

struct Captured<'a, 'i, G>(&'a G, &'a mut AsyncImageInput<'i>);

impl<G: PathIndexGraphs> Work for Captured<'_, '_, G> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.1.checkpoint().await?)
    }
}

impl<G: PathIndexGraphs> Graphs for Captured<'_, '_, G> {
    async fn contains(&mut self, graph: &str) -> StorageBackendResult<bool> {
        self.0.contains_from(graph, self.1).await
    }
}

fn missing() -> StorageBackendError {
    StorageBackendError::Other("path index references a missing graph".into())
}

fn disagrees() -> StorageBackendError {
    StorageBackendError::Other("recorded path-index owner disagrees with its key".into())
}

async fn select<'a>(
    raw: &'a str,
    recorded: Option<&str>,
    graphs: &mut impl Graphs,
    memory: &MemoryBudget,
) -> StorageBackendResult<Option<Identity<'a>>> {
    if let Some((graph, name)) = codec::decode(raw, memory, graphs).await? {
        if recorded.is_some_and(|owner| owner != graph.as_str()) {
            return Err(disagrees());
        }
        if !graphs.contains(&graph).await? {
            return Err(missing());
        }
        return Ok(Some(Identity {
            graph: Component::Decoded(graph),
            name: Component::Decoded(name),
            canonical: true,
        }));
    }
    let mut selected = None;
    let mut count = 0_usize;
    for step in (Separators { raw, offset: 0 }) {
        graphs.checkpoint().await?;
        let Some((graph, name)) = step else { continue };
        count += 1;
        if recorded.is_none_or(|owner| owner == graph) && graphs.contains(graph).await? {
            if selected.is_some() {
                return Err(StorageBackendError::Other(
                    "ambiguous path-index owner".into(),
                ));
            }
            selected = Some((graph, name));
        }
    }
    match selected {
        Some(pair) => Ok(Some(Identity::raw(pair, count == 1))),
        None if count == 0 => Ok(None),
        None if recorded.is_some_and(|graph| {
            !raw.strip_prefix(graph)
                .is_some_and(|tail| tail.starts_with("::"))
        }) =>
        {
            Err(disagrees())
        }
        None => Err(missing()),
    }
}

/// Decode a key that intrinsically identifies one pair. Ambiguous legacy and opaque keys require an owning catalog.
pub fn decode(key: &str) -> Option<(Cow<'_, str>, Cow<'_, str>)> {
    inline(select(
        key,
        None,
        &mut Catalog(|_: &str| Ok(true)),
        &MemoryBudget::new(key.len()),
    ))
    .ok()
    .flatten()
    .map(Identity::parts)
}

/// Resolve one physical key on the original snapshot. An opaque catalog key has no Engine pair.
pub fn resolve<'a>(
    key: &'a str,
    recorded_graph: Option<&str>,
    exists: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Option<(Cow<'a, str>, Cow<'a, str>)>> {
    Ok(inline(select(
        key,
        recorded_graph,
        &mut Catalog(exists),
        &MemoryBudget::new(key.len()),
    ))?
    .map(Identity::parts))
}

/// Determine whether a selective refresh needs to resolve a key against its original catalog.
pub fn may_belong_to(key: &str, mut selected: impl FnMut(&str) -> bool) -> bool {
    let mut candidates = legacy_candidates(key).peekable();
    if candidates.peek().is_some() {
        return candidates.any(|(graph, _)| selected(graph));
    }
    decode(key).is_some_and(|(graph, _)| selected(&graph))
}

/// Select definitions for a single graph inside its existing mutation. Unrelated and opaque physical keys do not participate in this mutation.
pub fn belongs_to(
    key: &str,
    graph: &str,
    recorded: impl FnOnce() -> StorageBackendResult<Option<String>>,
    exists: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<bool> {
    let raw_candidate = key
        .strip_prefix(graph)
        .is_some_and(|tail| tail.starts_with("::"));
    if !raw_candidate && !codec::encoded_for(key, graph) {
        return Ok(false);
    }
    Ok(resolve(key, recorded()?.as_deref(), exists)?.is_some_and(|(owner, _)| owner == graph))
}

/// Validate an original memory image address and retain its existing allocation whenever already canonical.
pub async fn canonicalized_from(
    key: Budgeted<String>,
    graphs: &impl PathIndexGraphs,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<String>> {
    if !key.budget().shares_allowance(input.memory) || key.reserved_bytes() < key.capacity() {
        return Err(StorageBackendError::Other(
            "path-index key lacks its retained input allocation".into(),
        ));
    }
    let memory = input.memory.clone();
    let identity = select(&key, None, &mut Captured(graphs, input), &memory)
        .await?
        .ok_or_else(|| StorageBackendError::Other("invalid path-index key".into()))?;
    if identity.canonical {
        return Ok(key);
    }
    codec::key_from(identity.graph.as_ref(), identity.name.as_ref(), input).await
}

#[cfg(test)]
mod tests;
