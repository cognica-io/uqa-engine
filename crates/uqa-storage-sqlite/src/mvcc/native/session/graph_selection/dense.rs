//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense label pages validate source identities and memberships with bounded range reads.

use super::{
    entity_family, owner, text, BudgetedVec, Family, GraphEntityFilter, GraphEntityKind,
    GraphEntitySelector, NativeRecordIdentity, NativeSnapshot, Result, Selection, ValueRef,
    VersionError,
};

pub(super) fn presence(
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
    selection: &Selection<'_>,
    ids: &[i64],
) -> Result<Option<BudgetedVec<u8>>> {
    let Some(graph) = filter.graph else {
        return Ok(None);
    };
    if filter.kind != GraphEntityKind::Vertex
        || !matches!(selection.selector, GraphEntitySelector::Label(_))
    {
        return Ok(None);
    }
    let offset = usize::from(selection.scope.is_some());
    let mut parts = [ValueRef::Null; 3];
    if let Some(scope) = selection.scope {
        parts[0] = text(scope);
    }
    let Some(mut flags) = snapshot.dense_identity_presence(
        entity_family(filter.kind, offset != 0),
        owner(snapshot),
        &parts[..offset],
        ids,
        &snapshot.control,
    )?
    else {
        return Ok(None);
    };
    parts[offset] = text(filter.kind.as_str());
    if !mark_membership(
        snapshot,
        if offset == 0 {
            Family::GraphMembership
        } else {
            Family::StandaloneGraphMembership
        },
        &parts[..=offset],
        ids,
        graph,
        &mut flags,
    )? {
        return Ok(None);
    }
    Ok(Some(flags))
}

fn mark_membership(
    snapshot: &NativeSnapshot,
    family: Family,
    components: &[ValueRef<'_>],
    ids: &[i64],
    graph: &str,
    flags: &mut [u8],
) -> Result<bool> {
    let identity = NativeRecordIdentity::new(family, owner(snapshot))?;
    let prefix = identity.encode_prefix(components, &snapshot.control)?;
    let mut lower = [ValueRef::Null; 4];
    lower[..components.len()].copy_from_slice(components);
    let after = ids[0]
        .checked_sub(1)
        .map(|before| {
            lower[components.len()] = ValueRef::Integer(before);
            identity.encode_prefix(&lower[..=components.len()], &snapshot.control)
        })
        .transpose()?;
    let last = *ids.last().expect("nonempty dense page");
    // A vertex can belong to many graphs. Cap metadata work before using point probes for high membership fanout or excessive retained tombstones.
    let limit = ids.len().saturating_mul(2).saturating_add(2);
    let mut visited = 0;
    let mut surpassed = false;
    snapshot.view.visit_keys(
        &prefix,
        after.as_deref(),
        limit,
        &snapshot.control,
        &mut |key, record| {
            visited += 1;
            let mut id = None;
            let mut in_graph = false;
            NativeRecordIdentity::visit_key_components(key, &snapshot.control, |column, value| {
                if column == components.len() {
                    id = Some(value.as_i64().map_err(|_| {
                        VersionError::InvalidEncoding("graph selector identity is not an integer")
                    })?);
                } else if column == components.len() + 1 {
                    in_graph = value.as_str().map_err(|_| {
                        VersionError::InvalidEncoding("graph membership name is not text")
                    })? == graph;
                }
                Ok(())
            })?;
            let id = id.ok_or(VersionError::InvalidEncoding(
                "graph source key is missing its identity",
            ))?;
            if id > last {
                surpassed = true;
                return Ok(false);
            }
            if record.live && in_graph {
                if let Ok(position) = ids.binary_search(&id) {
                    flags[position] |= 2;
                }
            }
            Ok(true)
        },
    )?;
    Ok(surpassed || visited < limit)
}
