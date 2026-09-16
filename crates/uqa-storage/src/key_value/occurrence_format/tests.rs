//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native projections use exactly the existing ordered byte identities and reject incomplete components.

use super::super::codec;
use super::*;
use crate::TokenTermKey;

#[test]
fn projected_addresses_preserve_existing_current_and_legacy_key_bytes() {
    let control = StorageReadControl::with_limit(4096);
    let table = "docs\0日本語";
    let field = "field\0é";
    let term = TokenTermKey::from_text("α");
    let mut expected = vec![keys::table_prefix(table).unwrap()];
    for kind in [keys::SCORE, keys::POSITIONS, keys::SKIP, keys::BLOCK_MAX] {
        expected.push(keys::kind_prefix(table, kind).unwrap());
        expected.push(keys::field_prefix(table, kind, field).unwrap());
        expected.push(keys::term_prefix(table, kind, field, &term).unwrap());
        expected.push(keys::cluster_key(table, kind, field, &term, u64::MAX).unwrap());
    }
    for kind in [keys::DOCUMENT, keys::LENGTH] {
        expected.push(keys::kind_prefix(table, kind).unwrap());
        expected.push(keys::document_prefix(table, kind, u64::MAX).unwrap());
        expected.push(keys::document_key(table, kind, u64::MAX, field).unwrap());
    }
    expected.push(keys::field_prefix(table, keys::METADATA, field).unwrap());
    expected.push(keys::metadata_key(table, field, u64::MAX).unwrap());
    expected.push(keys::field_prefix(table, keys::FIELD, "").unwrap());
    expected.push(keys::kind_prefix(table, keys::FORMAT).unwrap());
    for prefix in [
        codec::posting_key_prefix,
        codec::reverse_posting_key_prefix,
        codec::posting_cluster_score_key_prefix,
        codec::posting_cluster_positions_key_prefix,
        codec::posting_document_key_prefix,
        codec::doc_length_key_prefix,
        codec::field_stats_key_prefix,
    ] {
        expected.push(prefix(table).unwrap());
    }
    for bytes in expected {
        let address = OccurrenceAddress::decode(&bytes).unwrap();
        assert_eq!(&*address.encode(&control).unwrap(), bytes);
        assert_eq!(address.table, table);
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn projected_addresses_reject_bad_shapes_and_honor_caller_control() {
    let control = StorageReadControl::with_limit(4096);
    let base = OccurrenceAddress::table("docs");
    for address in [
        OccurrenceAddress {
            field: Some("x"),
            ..base
        },
        OccurrenceAddress {
            projection: Some(Score),
            cluster: Some(1),
            ..base
        },
        OccurrenceAddress {
            projection: Some(Length),
            field: Some("x"),
            ..base
        },
        OccurrenceAddress {
            projection: Some(Field),
            field: Some("x"),
            term: Some(b"term"),
            ..base
        },
        OccurrenceAddress {
            projection: Some(Format),
            document: Some(1),
            ..base
        },
        OccurrenceAddress {
            projection: Some(LegacyScore),
            field: Some("x"),
            ..base
        },
    ] {
        assert!(address.encode(&control).is_err(), "{address:?}");
        assert_eq!(control.memory().used(), 0);
    }
    let mut trailing = keys::metadata_key("docs", "body", 1).unwrap();
    trailing.push(0);
    let mut number = keys::document_prefix("docs", keys::DOCUMENT, 1).unwrap();
    number.pop();
    let mut unknown = keys::table_prefix("docs").unwrap();
    unknown.push(0xff);
    for malformed in [vec![], vec![0], trailing, number, unknown] {
        assert!(OccurrenceAddress::decode(&malformed).is_err());
    }
    let tiny = StorageReadControl::with_limit(1);
    assert!(matches!(
        base.encode(&tiny),
        Err(crate::StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        base.encode(&control),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
