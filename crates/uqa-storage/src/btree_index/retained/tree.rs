//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AVL paths retain Core's exact value/identity order while record roots own snapshots and spill.

use super::records::{invalid, Node, Records, MAX_HEIGHT};
use crate::{mvcc::PrivateRecordSnapshot, StorageBackendResult};
use std::{cmp::Ordering, ops::Bound};
use uqa_core::{DocId, Value};

fn check(records: &Records<'_>, depth: u8) -> StorageBackendResult<()> {
    records.control.check()?;
    if depth >= MAX_HEIGHT {
        return Err(invalid("ordered path exceeds the maximum AVL height"));
    }
    Ok(())
}

pub(super) fn insert(
    records: &Records<'_>,
    root: Option<DocId>,
    id: DocId,
    value: &Value,
    depth: u8,
) -> StorageBackendResult<DocId> {
    check(records, depth)?;
    let Some(current) = root else { return Ok(id) };
    let mut node = records.node(current)?;
    let stored = records
        .get(&records.snapshot()?, current)?
        .ok_or_else(|| invalid("missing comparison value"))?;
    let order = value.cmp(&stored).then_with(|| id.cmp(&current));
    drop(stored);
    match order {
        Ordering::Less => node.left = Some(insert(records, node.left, id, value, depth + 1)?),
        Ordering::Greater => node.right = Some(insert(records, node.right, id, value, depth + 1)?),
        Ordering::Equal => return Err(invalid("duplicate ordered identity")),
    }
    balance(records, current, node)
}

pub(super) fn remove(
    records: &Records<'_>,
    root: Option<DocId>,
    id: DocId,
    value: &Value,
    depth: u8,
) -> StorageBackendResult<Option<DocId>> {
    check(records, depth)?;
    let current = root.ok_or_else(|| invalid("missing deletion path"))?;
    let mut node = records.node(current)?;
    let stored = records
        .get(&records.snapshot()?, current)?
        .ok_or_else(|| invalid("missing comparison value"))?;
    let order = value.cmp(&stored).then_with(|| id.cmp(&current));
    drop(stored);
    match order {
        Ordering::Less => node.left = remove(records, node.left, id, value, depth + 1)?,
        Ordering::Greater => node.right = remove(records, node.right, id, value, depth + 1)?,
        Ordering::Equal => {
            let Some(right) = node.right else {
                return Ok(node.left);
            };
            let (successor, remaining) = take_min(records, right, depth + 1)?;
            let mut replacement = records.node(successor)?;
            replacement.left = node.left;
            replacement.right = remaining;
            return balance(records, successor, replacement).map(Some);
        }
    }
    balance(records, current, node).map(Some)
}

fn take_min(
    records: &Records<'_>,
    id: DocId,
    depth: u8,
) -> StorageBackendResult<(DocId, Option<DocId>)> {
    check(records, depth)?;
    let mut node = records.node(id)?;
    let Some(left) = node.left else {
        return Ok((id, node.right));
    };
    let (minimum, remaining) = take_min(records, left, depth + 1)?;
    node.left = remaining;
    Ok((minimum, Some(balance(records, id, node)?)))
}

fn save_height(records: &Records<'_>, id: DocId, mut node: Node) -> StorageBackendResult<()> {
    node.height = records
        .height(node.left)?
        .max(records.height(node.right)?)
        .checked_add(1)
        .filter(|height| *height <= MAX_HEIGHT)
        .ok_or_else(|| invalid("ordered height overflow"))?;
    node.count = records
        .count(node.left)?
        .checked_add(records.count(node.right)?)
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| invalid("ordered count overflow"))?;
    records.set_node(id, node)
}

fn balance(records: &Records<'_>, id: DocId, mut node: Node) -> StorageBackendResult<DocId> {
    let left = records.height(node.left)?;
    let right = records.height(node.right)?;
    if left > right + 1 {
        let child = node
            .left
            .ok_or_else(|| invalid("missing left rotation child"))?;
        let child_node = records.node(child)?;
        if records.height(child_node.right)? > records.height(child_node.left)? {
            node.left = Some(rotate_left(records, child, child_node)?);
        }
        return rotate_right(records, id, node);
    }
    if right > left + 1 {
        let child = node
            .right
            .ok_or_else(|| invalid("missing right rotation child"))?;
        let child_node = records.node(child)?;
        if records.height(child_node.left)? > records.height(child_node.right)? {
            node.right = Some(rotate_right(records, child, child_node)?);
        }
        return rotate_left(records, id, node);
    }
    save_height(records, id, node)?;
    Ok(id)
}

fn rotate_left(records: &Records<'_>, id: DocId, mut node: Node) -> StorageBackendResult<DocId> {
    let right = node
        .right
        .ok_or_else(|| invalid("missing left rotation pivot"))?;
    let mut pivot = records.node(right)?;
    node.right = pivot.left;
    pivot.left = Some(id);
    save_height(records, id, node)?;
    save_height(records, right, pivot)?;
    Ok(right)
}
fn rotate_right(records: &Records<'_>, id: DocId, mut node: Node) -> StorageBackendResult<DocId> {
    let left = node
        .left
        .ok_or_else(|| invalid("missing right rotation pivot"))?;
    let mut pivot = records.node(left)?;
    node.left = pivot.right;
    pivot.right = Some(id);
    save_height(records, id, node)?;
    save_height(records, left, pivot)?;
    Ok(left)
}

pub(super) fn visit(
    records: &Records<'_>,
    snapshot: &PrivateRecordSnapshot,
    root: Option<DocId>,
    low: Bound<&Value>,
    high: Bound<&Value>,
    visit: &mut dyn FnMut(DocId) -> StorageBackendResult<bool>,
    depth: u8,
) -> StorageBackendResult<bool> {
    check(records, depth)?;
    let Some(id) = root else { return Ok(true) };
    let node = records
        .node_at(snapshot, id)?
        .ok_or_else(|| invalid("missing read path"))?;
    let value = records
        .get(snapshot, id)?
        .ok_or_else(|| invalid("missing range value"))?;
    let above = match low {
        Bound::Included(low) => value >= *low,
        Bound::Excluded(low) => value > *low,
        Bound::Unbounded => true,
    };
    let below = match high {
        Bound::Included(high) => value <= *high,
        Bound::Excluded(high) => value < *high,
        Bound::Unbounded => true,
    };
    drop(value);
    if above && !self::visit(records, snapshot, node.left, low, high, visit, depth + 1)? {
        return Ok(false);
    }
    if above && below && !visit(id)? {
        return Ok(false);
    }
    if below && !self::visit(records, snapshot, node.right, low, high, visit, depth + 1)? {
        return Ok(false);
    }
    Ok(true)
}

/// Count a strict/inclusive prefix using subtree counts, without scanning its matching population.
pub(super) fn rank(
    records: &Records<'_>,
    mut root: Option<DocId>,
    value: &Value,
    inclusive: bool,
) -> StorageBackendResult<u64> {
    let snapshot = records.snapshot()?;
    let mut count = 0_u64;
    let mut depth = 0;
    while let Some(id) = root {
        check(records, depth)?;
        depth += 1;
        let node = records
            .node_at(&snapshot, id)?
            .ok_or_else(|| invalid("missing rank path"))?;
        let stored = records
            .get(&snapshot, id)?
            .ok_or_else(|| invalid("missing rank value"))?;
        let order = stored.cmp(value);
        drop(stored);
        if order.is_lt() || (inclusive && order.is_eq()) {
            count = count
                .checked_add(records.count(node.left)?)
                .and_then(|count| count.checked_add(1))
                .ok_or_else(|| invalid("rank count overflow"))?;
            root = node.right;
        } else {
            root = node.left;
        }
    }
    Ok(count)
}

pub(super) struct Builder<'a, 'b> {
    pub records: &'a Records<'b>,
    pub source: &'a mut dyn FnMut() -> StorageBackendResult<Option<(DocId, Value)>>,
    pub previous: Option<(DocId, Value)>,
}
impl Builder<'_, '_> {
    pub fn build(&mut self, count: usize, depth: u8) -> StorageBackendResult<(Option<DocId>, u8)> {
        check(self.records, depth)?;
        if count == 0 {
            return Ok((None, 0));
        }
        let left_count = count / 2;
        let (left, left_height) = self.build(left_count, depth + 1)?;
        let (id, value) = (self.source)()?.ok_or_else(|| invalid("short ordered index input"))?;
        if self.previous.as_ref().is_some_and(|(before_id, before)| {
            before.cmp(&value).then_with(|| before_id.cmp(&id)).is_ge()
        }) {
            return Err(invalid("unordered index input"));
        }
        if self
            .records
            .node_at(&self.records.snapshot()?, id)?
            .is_some()
        {
            return Err(invalid("duplicate index input identity"));
        }
        let bytes = self.records.put_value(id, &value)?;
        self.previous = Some((id, value));
        let (right, right_height) = self.build(count - left_count - 1, depth + 1)?;
        let height = left_height
            .max(right_height)
            .checked_add(1)
            .filter(|height| *height <= MAX_HEIGHT)
            .ok_or_else(|| invalid("ordered height overflow"))?;
        // This unpublished builder just completed both children, so their metadata need not be read from spill again.
        self.records.set_node(
            id,
            Node {
                left,
                right,
                height,
                bytes,
                count: count as u64,
            },
        )?;
        Ok((Some(id), height))
    }
}
