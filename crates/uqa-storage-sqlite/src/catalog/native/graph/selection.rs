//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fetch a bounded page of selector keys, release the physical read, then test the remaining predicates through the same retained view.

use uqa_core::memory::BudgetedVec;
use uqa_storage::{catalog::GraphEntitySelector, mvcc::VersionError};

use super::{
    encode_catalog_id, entity_family, owner, text, Family, GraphEntityFilter, NativeSnapshot,
    Result, ValueRef,
};
use crate::mvcc::native::NativeRecordIdentity;

struct Selection<'a> {
    family: Family,
    selector: GraphEntitySelector<'a>,
    parts: [ValueRef<'a>; 4],
    width: usize,
}

impl<'a> Selection<'a> {
    fn new(filter: GraphEntityFilter<'a>) -> Result<Self> {
        let selector = filter.selector().map_err(crate::SQLiteError::from)?;
        let source = filter
            .source
            .map(|id| encode_catalog_id("edge source", id))
            .transpose()?;
        let target = filter
            .target
            .map(|id| encode_catalog_id("edge target", id))
            .transpose()?;
        let kind = text(filter.kind.as_str());
        let zero = ValueRef::Integer(0);
        let parts = match selector {
            GraphEntitySelector::Source(_) => [
                text("source"),
                text(""),
                ValueRef::Integer(source.expect("selected source")),
                kind,
            ],
            GraphEntitySelector::Target(_) => [
                text("target"),
                text(""),
                ValueRef::Integer(target.expect("selected target")),
                kind,
            ],
            GraphEntitySelector::Label(label) => [text("label"), text(label), zero, kind],
            GraphEntitySelector::Graph(graph) => [text("member"), text(graph), zero, kind],
            GraphEntitySelector::All => [zero; 4],
        };
        Ok(Self {
            family: if selector == GraphEntitySelector::All {
                entity_family(filter.kind)
            } else {
                Family::GraphLookups
            },
            selector,
            parts,
            width: if selector == GraphEntitySelector::All {
                0
            } else {
                4
            },
        })
    }

    fn page(
        &self,
        snapshot: &NativeSnapshot,
        after: Option<i64>,
    ) -> Result<(BudgetedVec<i64>, Option<i64>)> {
        let identity = NativeRecordIdentity::new(self.family, owner(snapshot))?;
        let prefix = identity.encode_prefix(&self.parts[..self.width], &snapshot.control)?;
        let mut components = [ValueRef::Null; 5];
        components[..self.width].copy_from_slice(&self.parts[..self.width]);
        let cursor = after
            .map(|after| {
                components[self.width] = ValueRef::Integer(after);
                identity.encode_key(&components[..=self.width], &snapshot.control)
            })
            .transpose()?;
        let mut ids = BudgetedVec::new(snapshot.control.memory());
        let mut last = None;
        snapshot.view.visit_keys(
            &prefix,
            cursor.as_deref(),
            256,
            &snapshot.control,
            &mut |key, record| {
                NativeRecordIdentity::visit_key_components(
                    key,
                    &snapshot.control,
                    |column, value| {
                        if column == self.width {
                            let id = value.as_i64().map_err(|_| {
                                VersionError::InvalidEncoding(
                                    "graph selector identity is not an integer",
                                )
                            })?;
                            last = Some(id);
                            if record.live {
                                ids.push(id)?;
                            }
                        }
                        Ok(())
                    },
                )?;
                Ok(true)
            },
        )?;
        Ok((ids, last))
    }
}

pub(super) fn visit(
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
    after: Option<u64>,
    mut visit: impl FnMut(i64) -> Result<bool>,
) -> Result<()> {
    let selection = Selection::new(filter)?;
    let mut after = after
        .map(|id| encode_catalog_id("graph scan cursor", id))
        .transpose()?;
    loop {
        let (ids, last) = selection.page(snapshot, after)?;
        let Some(last) = last else {
            break;
        };
        after = Some(last);
        for id in ids.iter().copied() {
            if !matches(snapshot, filter, &selection, id)? {
                continue;
            }
            if !visit(id)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn matches(
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
    selection: &Selection<'_>,
    id: i64,
) -> Result<bool> {
    let encoded = ValueRef::Integer(id);
    if selection.family == Family::GraphLookups
        && !snapshot.contains_row(entity_family(filter.kind), owner(snapshot), &[encoded])?
    {
        return Err(crate::SQLiteError::StorageBackend(format!(
            "graph lookup references missing {} {id}",
            filter.kind.as_str()
        )));
    }
    let lookup = |kind, key, integer| {
        snapshot.contains_row(
            Family::GraphLookups,
            owner(snapshot),
            &[
                text(kind),
                text(key),
                ValueRef::Integer(integer),
                text(filter.kind.as_str()),
                encoded,
            ],
        )
    };
    if let Some(label) = filter.label {
        if selection.selector != GraphEntitySelector::Label(label) && !lookup("label", label, 0)? {
            return Ok(false);
        }
    }
    if let Some(source) = filter.source {
        if selection.selector != GraphEntitySelector::Source(source)
            && !lookup("source", "", encode_catalog_id("edge source", source)?)?
        {
            return Ok(false);
        }
    }
    if let Some(target) = filter.target {
        if selection.selector != GraphEntitySelector::Target(target)
            && !lookup("target", "", encode_catalog_id("edge target", target)?)?
        {
            return Ok(false);
        }
    }
    if let Some(graph) = filter.graph {
        if selection.selector != GraphEntitySelector::Graph(graph)
            && !snapshot.contains_row(
                Family::GraphMembership,
                owner(snapshot),
                &[text(filter.kind.as_str()), encoded, text(graph)],
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
}
