//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained whole-field streams and real-origin build inputs across Key/Value providers.

use super::{expect, expect_eq};
use crate::diskann_index::{
    build::{DiskANNBuildInput, DiskANNTemporaryBudget},
    format::DiskANNGeneration,
    DiskANNCanonicalRead,
};
use crate::key_value::{KeyValueDiskANNCanonical, KeyValueVectorIndex, RetainedDiskANNCanonical};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult, VectorIndex};
use std::sync::Arc;

fn index(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<KeyValueDiskANNCanonical> {
    KeyValueDiskANNCanonical::new(store.clone(), "diskann-corpus", "embedding", 2)
}

pub(super) fn verify(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let index = index(store)?;
    let absent = index.retain(&control)?;
    expect_eq(
        &absent.next_document_after(None, &control)?,
        &None,
        "empty corpus",
    )?;
    let high = index.replace(u64::MAX, &[vec![f32::MAX, 0.0]], &control)?;
    let empty = index.replace(7, &[], &control)?;
    let low = index.replace(0, &[vec![3.0, 4.0], vec![-0.0, 0.0]], &control)?;
    let fixed = index.retain(&control)?;
    expect_eq(&fixed.dimensions(), &2, "retained corpus dimensions")?;
    expect_eq(
        &absent.next_document_after(None, &control)?,
        &None,
        "absent source stays absent",
    )?;
    let directory = tempfile::tempdir()
        .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let query = StorageReadControl::with_limit(8192);
    let input = DiskANNBuildInput::capture_source(
        DiskANNGeneration::new([17; 16], 1, 2, 3)?,
        &fixed,
        directory.path(),
        &temporary,
        &query,
    )?;
    expect_eq(
        &(input.node_count(), input.side_count()),
        &(1, 2),
        "real canonical build classification",
    )?;
    expect_eq(
        &input.coverage().vector_count(),
        &3,
        "complete canonical build coverage count",
    )?;
    let node = input.read_node(0)?;
    expect_eq(
        &(node.doc_id(), node.ordinal(), node.version()),
        &(0, 0, low),
        "build keeps real mutation origin",
    )?;
    expect(node.raw() == [3.0, 4.0], "canonical build raw values")?;
    drop(node);
    let zero = input.read_side(0)?;
    expect_eq(
        &(zero.doc_id(), zero.ordinal(), zero.version()),
        &(0, 1, low),
        "zero side origin",
    )?;
    expect_eq(
        &zero.raw()[0].to_bits(),
        &0x8000_0000,
        "signed zero bits survive corpus capture",
    )?;
    drop(zero);
    let overflow = input.read_side(1)?;
    expect_eq(
        &(overflow.doc_id(), overflow.version()),
        &(u64::MAX, high),
        "last document is not lost",
    )?;
    drop(overflow);
    drop(input);
    expect_eq(&temporary.used(), &0, "capture temporary files released")?;
    expect_eq(&query.memory().used(), &0, "capture workspace released")?;
    store.begin_transaction()?;
    index.replace(0, &[], &control)?;
    index.replace(3, &[vec![1.0, 0.0]], &control)?;
    let private = index.retain(&control)?;
    store.rollback_transaction()?;
    let mut private_documents = Vec::new();
    private.visit_all(&query, &mut |doc, _, _, _| {
        private_documents.push(doc);
        Ok(())
    })?;
    expect_eq(
        &private_documents,
        &vec![3, u64::MAX],
        "undone private corpus remains fixed",
    )?;
    expect_eq(
        &fixed.origin(7, &query)?,
        &Some(empty),
        "empty replacement retained",
    )?;
    verify_reopen(store)?;
    verify_failures(store, &index, &fixed, &control)
}

fn verify_failures(
    store: &Arc<dyn KeyValueStore>,
    index: &KeyValueDiskANNCanonical,
    fixed: &RetainedDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let directory = tempfile::tempdir()
        .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let query = StorageReadControl::with_limit(8192);
    store.begin_transaction()?;
    let mut legacy = KeyValueVectorIndex::new(store.clone(), "diskann-corpus", "embedding", 2);
    legacy.add(1, vec![1.0, 0.0])?;
    let unversioned = index.retain(control)?;
    expect_eq(
        &unversioned.next_document_after(Some(0), &query)?,
        &Some(1),
        "unstamped document is enumerated",
    )?;
    expect(
        DiskANNBuildInput::capture_source(
            DiskANNGeneration::new([17; 16], 1, 2, 4)?,
            &unversioned,
            directory.path(),
            &temporary,
            &query,
        )
        .is_err(),
        "capture rejects unstamped canonical data",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "rejected capture releases partial files",
    )?;
    expect(
        std::fs::read_dir(directory.path())
            .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?
            .next()
            .is_none(),
        "no partial capture survives",
    )?;
    store.rollback_transaction()?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        fixed.next_document_after(Some(0), &tiny).is_err(),
        "corpus keys share invoking allowance",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed corpus key workspace released",
    )?;
    let cancelled = StorageReadControl::with_limit(8192);
    let mut calls = 0;
    expect(
        fixed
            .visit_all(&cancelled, &mut |_, _, _, _| {
                calls += 1;
                cancelled.cancellation().cancel();
                Ok(())
            })
            .is_err(),
        "corpus cancellation propagates",
    )?;
    expect_eq(&calls, &1, "cancelled corpus stops at first ordinal")?;
    expect_eq(
        &cancelled.memory().used(),
        &0,
        "cancelled corpus releases workspace",
    )?;
    control.cancellation().cancel();
    expect(
        fixed.next_document_after(Some(u64::MAX), &query).is_err(),
        "exhausted corpus still checks original cancellation",
    )
}

pub(super) fn verify_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let fixed = index(store)?.retain(&control)?;
    let query = StorageReadControl::with_limit(8192);
    let mut after = None;
    for expected in [0, 7, u64::MAX] {
        expect_eq(
            &fixed.next_document_after(after, &query)?,
            &Some(expected),
            "ordered distinct corpus identities including empty replacements",
        )?;
        after = Some(expected);
    }
    expect_eq(
        &fixed.next_document_after(after, &query)?,
        &None,
        "maximum document terminates without overflow",
    )?;
    let mut identities = Vec::new();
    fixed.visit_all(&query, &mut |doc, ordinal, _, _| {
        identities.push((doc, ordinal));
        Ok(())
    })?;
    expect_eq(
        &identities,
        &vec![(0, 0), (0, 1), (u64::MAX, 0)],
        "complete ordered canonical corpus",
    )?;
    expect_eq(
        &query.memory().used(),
        &0,
        "whole-corpus workspace released",
    )
}
