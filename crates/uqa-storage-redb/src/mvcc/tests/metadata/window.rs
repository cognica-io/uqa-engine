//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical table operations required to admit a snapshot read window.

use super::*;
use redb::{AccessGuard, Range, ReadOnlyTable, ReadableTableMetadata, TableStats};
use std::{borrow::Borrow, cell::Cell, ops::RangeBounds};

type Entry<'a> = (
    AccessGuard<'a, &'static str>,
    AccessGuard<'a, &'static [u8]>,
);

struct CountedMetadata {
    table: ReadOnlyTable<&'static str, &'static [u8]>,
    point_reads: Cell<usize>,
    range_reads: Cell<usize>,
}

impl ReadableTableMetadata for CountedMetadata {
    fn stats(&self) -> redb::Result<TableStats> {
        self.table.stats()
    }

    fn len(&self) -> redb::Result<u64> {
        self.table.len()
    }
}

impl ReadableTable<&'static str, &'static [u8]> for CountedMetadata {
    fn get<'a>(
        &self,
        key: impl Borrow<&'a str>,
    ) -> redb::Result<Option<AccessGuard<'_, &'static [u8]>>> {
        self.point_reads.set(self.point_reads.get() + 1);
        self.table.get(key)
    }

    fn range<'a, K>(
        &self,
        range: impl RangeBounds<K> + 'a,
    ) -> redb::Result<Range<'_, &'static str, &'static [u8]>>
    where
        K: Borrow<&'a str> + 'a,
    {
        self.range_reads.set(self.range_reads.get() + 1);
        self.table.range(range)
    }

    fn first(&self) -> redb::Result<Option<Entry<'_>>> {
        self.table.first()
    }

    fn last(&self) -> redb::Result<Option<Entry<'_>>> {
        self.table.last()
    }
}

#[test]
fn snapshot_metadata_uses_one_range_and_preserves_header_error_order() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let expected = RedbRecordStore::new(Arc::clone(&database))
        .unwrap()
        .database_id();
    let read = database.begin_read().unwrap();
    let metadata = CountedMetadata {
        table: read.open_table(METADATA).unwrap(),
        point_reads: Cell::new(0),
        range_reads: Cell::new(0),
    };
    codec::validate_metadata(&metadata, expected).unwrap();
    assert_eq!(
        (metadata.range_reads.get(), metadata.point_reads.get()),
        (1, 0)
    );
    drop((metadata, read));

    let write = physical_writer(&database).unwrap();
    let mut table = write.open_table(METADATA).unwrap();
    table
        .insert("receipt_limit", 0_u64.to_be_bytes().as_slice())
        .unwrap();
    table.remove("database").unwrap();
    table.remove("format").unwrap();
    assert!(matches!(
        codec::validate_metadata(&table, expected),
        Err(VersionError::InvalidEncoding("missing MVCC metadata"))
    ));
    table
        .insert("format", RECORD_FORMAT.to_be_bytes().as_slice())
        .unwrap();
    assert!(matches!(
        codec::validate_metadata(&table, expected),
        Err(VersionError::InvalidEncoding("missing database identity"))
    ));
    table
        .insert("database", expected.as_bytes().as_slice())
        .unwrap();
    assert!(matches!(
        codec::validate_metadata(&table, expected),
        Err(VersionError::InvalidEncoding(
            "invalid receipt retention limit"
        ))
    ));
}
