//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same reservation schedules exercise reference and physical persistence owners.

use std::{num::NonZeroU64, sync::Barrier};

use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};

use super::IdentifierRequest;
use crate::mvcc::{VersionError, VersionResult, VersionedPersistence};

fn reserve(minimum: u64, maximum: u64, count: u64) -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum,
        maximum,
        count: NonZeroU64::new(count).expect("nonzero conformance reservation"),
    }
}

fn require(valid: bool, reason: &'static str) -> VersionResult<()> {
    if valid {
        Ok(())
    } else {
        Err(VersionError::InvalidEncoding(reason))
    }
}

/// Exercise the evaluated batch boundary through two sessions of one disposable byte store. The returned watermark belongs to namespace `identifier-batches` and must survive close/reopen.
pub fn verify_identifier_batches(
    a: &dyn KeyValueStore,
    b: &dyn KeyValueStore,
) -> StorageBackendResult<u64> {
    let ids = b.identifier_allocator().expect("durable byte store");
    let namespace = b"identifier-batches";
    let reader = a.identifier_allocator().expect("durable byte store");
    assert_eq!(reader.identifier_watermark(namespace)?, None);
    assert!(!a.in_transaction());
    let mut dropped = a.batch();
    dropped.observe_identifier(namespace, 999)?;
    drop(dropped);
    a.begin_transaction()?;
    a.put(b"batch-prior", b"private")?;
    a.savepoint("before-observation")?;
    a.with_mutation(&mut |read, batch| {
        assert_eq!(read.get(b"batch-prior")?.as_deref(), Some(&b"private"[..]));
        batch.observe_identifier(namespace, 100)?;
        batch.put(b"batch-row", b"private")
    })?;
    assert_eq!(
        ids.allocate_identifiers(namespace, reserve(1, u64::MAX, 1))?
            .watermark(),
        101
    );
    assert!(b.get(b"batch-row")?.is_none());
    b.put(b"batch-other", b"committed")?;
    assert!(a.in_transaction());
    a.rollback_to_savepoint("before-observation")?;
    assert!(a.get(b"batch-row")?.is_none());
    assert_eq!(a.get(b"batch-prior")?.as_deref(), Some(&b"private"[..]));
    assert_eq!(
        ids.allocate_identifiers(namespace, reserve(1, u64::MAX, 1))?
            .watermark(),
        102
    );
    a.rollback_transaction()?;
    assert!(a.get(b"batch-prior")?.is_none());
    a.begin_read_transaction()?;
    assert_eq!(reader.identifier_watermark(namespace)?, Some(102));
    assert_eq!(reader.identifier_watermark(b"absent-watermark")?, None);
    ids.allocate_identifiers(b"read-only-watermark", reserve(0, u64::MAX, 1))?;
    assert_eq!(
        reader.identifier_watermark(b"read-only-watermark")?,
        Some(0)
    );
    assert!(a.in_transaction());
    let mut rejected = a.batch();
    rejected.observe_identifier(namespace, 999)?;
    assert!(rejected.commit().is_err());
    a.rollback_transaction()?;
    let last = ids
        .allocate_identifiers(namespace, reserve(1, u64::MAX, 1))?
        .watermark();
    assert_eq!(last, 103);
    let mut full = a.batch();
    full.observe_identifier(b"identifier-batches-full", u64::MAX)?;
    full.commit()?;
    assert!(ids
        .allocate_identifiers(b"identifier-batches-full", reserve(1, u64::MAX, 1))
        .is_err());
    Ok(last)
}

/// Exercise two owners of the same fresh, disposable database. Reserved conformance namespaces are intentionally retained, because rolling back or clearing record data must never recycle identifiers.
pub fn verify_identifier_allocations(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    require(
        a.database_id() == b.database_id(),
        "identifier owners belong to different databases",
    )?;
    let snapshot = a.snapshot(control)?;
    let transaction = a.allocate_transaction(control)?;
    a.abort(transaction, control)?;
    let retained = control.memory().used();
    verify_ranges(a, b, control)?;
    verify_limits(a, b, control)?;
    verify_admission(a, b, control)?;
    verify_contended_reservations(a, b, control)?;
    verify_watermark_reads(a, b, control)?;
    require(
        control.memory().used() == retained,
        "identifier allocation leaked its workspace",
    )?;
    require(
        a.snapshot(control)?.sequence() == snapshot.sequence(),
        "identifier reservation advanced record visibility",
    )?;
    let after = b.allocate_transaction(control)?;
    require(
        after.allocation() == transaction.allocation() + 1,
        "identifier reservation consumed a transaction allocation",
    )?;
    b.abort(after, control)?;
    Ok(())
}

fn verify_watermark_reads(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let namespace = b"\0uqa-identifier-conformance\0reads";
    require(
        a.identifier_watermark(namespace, control)?.is_none()
            && b.identifier_watermark(namespace, control)?.is_none(),
        "absent identifier read created a namespace",
    )?;
    require(
        b.allocate_identifiers(namespace, reserve(0, u64::MAX, 1), control)?
            .watermark()
            == 0,
        "watermark read consumed the first identity",
    )?;
    require(
        a.identifier_watermark(namespace, control)? == Some(0),
        "zero watermark was confused with absence",
    )?;
    b.allocate_identifiers(namespace, IdentifierRequest::Observe(u64::MAX), control)?;
    require(
        a.identifier_watermark(namespace, control)? == Some(u64::MAX),
        "watermark read missed an independent reservation or truncated its value",
    )?;
    require(
        a.identifier_watermark(b"", control).is_err(),
        "empty identifier read namespace was accepted",
    )?;
    require(
        matches!(
            a.identifier_watermark(namespace, &StorageReadControl::with_limit(1)),
            Err(VersionError::Memory(_))
        ),
        "watermark read ignored its memory allowance",
    )?;
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    require(
        matches!(
            a.identifier_watermark(namespace, &cancelled),
            Err(VersionError::Cancelled(_))
        ),
        "watermark read ignored cancellation",
    )
}

fn verify_ranges(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let namespace = b"\0uqa-identifier-conformance\0range";
    require(
        a.allocate_identifiers(namespace, reserve(0, u64::MAX, 2), control)?
            .range()
            == Some(0..=1),
        "initial range did not include zero",
    )?;
    b.allocate_identifiers(namespace, IdentifierRequest::Observe(4096), control)?;
    require(
        b.allocate_identifiers(namespace, reserve(0, u64::MAX, 2), control)?
            .range()
            == Some(4097..=4098),
        "observation was not visible to the other owner",
    )?;
    a.allocate_identifiers(namespace, IdentifierRequest::Observe(1), control)?;
    require(
        a.allocate_identifiers(namespace, reserve(0, u64::MAX, 1), control)?
            .range()
            == Some(4099..=4099),
        "observation rewound the watermark",
    )?;
    require(
        a.allocate_identifiers(
            b"\0uqa-identifier-conformance\0range\0",
            reserve(0, u64::MAX, 1),
            control,
        )?
        .range()
            == Some(0..=0),
        "prefix-related namespaces share a watermark",
    )?;

    Ok(())
}

fn verify_limits(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let maximum = b"\0uqa-identifier-conformance\0maximum";
    a.allocate_identifiers(maximum, IdentifierRequest::Observe(u64::MAX - 1), control)?;
    require(
        b.allocate_identifiers(maximum, reserve(0, u64::MAX, 1), control)?
            .range()
            == Some(u64::MAX..=u64::MAX),
        "full-width final identifier was lost",
    )?;
    require(
        matches!(
            a.allocate_identifiers(maximum, reserve(0, u64::MAX, 1), control),
            Err(VersionError::IdentifiersExhausted)
        ),
        "exhausted namespace reused an identifier",
    )?;

    let bounded = b"\0uqa-identifier-conformance\0bounded";
    a.allocate_identifiers(bounded, IdentifierRequest::Observe(8), control)?;
    require(
        matches!(
            b.allocate_identifiers(bounded, reserve(0, 10, 3), control),
            Err(VersionError::IdentifiersExhausted)
        ),
        "oversized reservation partially succeeded",
    )?;
    require(
        a.allocate_identifiers(bounded, reserve(0, 10, 1), control)?
            .range()
            == Some(9..=9),
        "failed reservation changed its watermark",
    )?;

    Ok(())
}

fn verify_admission(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let quota = b"\0uqa-identifier-conformance\0quota";
    require(
        matches!(
            a.allocate_identifiers(
                quota,
                reserve(1, u64::MAX, 1),
                &StorageReadControl::with_limit(1)
            ),
            Err(VersionError::Memory(_))
        ),
        "identifier key ignored its memory allowance",
    )?;
    require(
        b.allocate_identifiers(quota, reserve(1, u64::MAX, 1), control)?
            .range()
            == Some(1..=1),
        "failed admission consumed an identifier",
    )?;
    require(
        a.allocate_identifiers(b"", reserve(1, 9, 1), control)
            .is_err(),
        "empty allocation namespace was accepted",
    )?;

    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    let namespace = b"\0uqa-identifier-conformance\0cancelled";
    require(
        matches!(
            a.allocate_identifiers(namespace, reserve(1, 9, 1), &cancelled),
            Err(VersionError::Cancelled(_))
        ),
        "identifier reservation ignored cancellation",
    )?;
    require(
        b.allocate_identifiers(namespace, reserve(1, 9, 1), control)?
            .range()
            == Some(1..=1),
        "cancelled reservation consumed an identifier",
    )?;
    Ok(())
}

fn verify_contended_reservations(
    a: &dyn VersionedPersistence,
    b: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let barrier = Barrier::new(2);
    let ranges = std::thread::scope(|scope| {
        let request = || {
            barrier.wait();
            a.allocate_identifiers(
                b"\0uqa-identifier-conformance\0contended",
                reserve(1, 8, 4),
                control,
            )
        };
        let first = scope.spawn(request);
        barrier.wait();
        let second = b.allocate_identifiers(
            b"\0uqa-identifier-conformance\0contended",
            reserve(1, 8, 4),
            control,
        );
        (
            first
                .join()
                .expect("identifier conformance worker panicked"),
            second,
        )
    });
    let mut starts = [
        *ranges.0?.range().expect("reserved range").start(),
        *ranges.1?.range().expect("reserved range").start(),
    ];
    starts.sort_unstable();
    require(
        starts == [1, 5],
        "concurrent reservations overlap or lose a range",
    )?;
    Ok(())
}
