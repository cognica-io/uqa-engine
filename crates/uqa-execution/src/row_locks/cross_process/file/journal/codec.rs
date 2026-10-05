//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width row-change event encoding.

use super::super::{
    LockStrength, PublishedRowChange, PublishedRowChangeKind, PublishedRowIdentity,
    CHANGE_ENTRY_MAGIC, CHANGE_ENTRY_SIZE,
};

pub(super) fn encode_change_entry(
    sequence: u64,
    change: &PublishedRowChange,
) -> [u8; CHANGE_ENTRY_SIZE as usize] {
    let mut entry = [0_u8; CHANGE_ENTRY_SIZE as usize];
    entry[0..4].copy_from_slice(&CHANGE_ENTRY_MAGIC.to_be_bytes());
    let (kind, successor) = match change.kind {
        PublishedRowChangeKind::Update => (
            1,
            PublishedRowIdentity {
                table_hash: 0,
                doc_id: 0,
            },
        ),
        PublishedRowChangeKind::Delete => (
            2,
            PublishedRowIdentity {
                table_hash: 0,
                doc_id: 0,
            },
        ),
        PublishedRowChangeKind::Rewrite(successor) => (3, successor),
    };
    entry[4] = kind;
    entry[5] = strength_code(change.strength);
    entry[8..16].copy_from_slice(&sequence.wrapping_add(1).to_be_bytes());
    entry[16..24].copy_from_slice(&change.table_hash.to_be_bytes());
    entry[24..32].copy_from_slice(&change.doc_id.to_be_bytes());
    entry[32..40].copy_from_slice(&successor.doc_id.to_be_bytes());
    entry[40..48].copy_from_slice(&successor.table_hash.to_be_bytes());
    entry
}

pub(super) fn decode_change_entry(
    sequence: u64,
    entry: &[u8; CHANGE_ENTRY_SIZE as usize],
) -> Result<PublishedRowChange, String> {
    if entry[0..4] != CHANGE_ENTRY_MAGIC.to_be_bytes() {
        return Err(format!(
            "row-change journal entry {sequence} has invalid magic"
        ));
    }
    let stored_sequence = u64::from_be_bytes(
        entry[8..16]
            .try_into()
            .map_err(|_| format!("decode row-change journal sequence for entry {sequence}"))?,
    );
    if stored_sequence != sequence.wrapping_add(1) {
        return Err(format!(
            "row-change journal entry {sequence} changed while it was read"
        ));
    }
    let table_hash = u64::from_be_bytes(
        entry[16..24]
            .try_into()
            .map_err(|_| format!("decode row-change table for entry {sequence}"))?,
    );
    let doc_id = u64::from_be_bytes(
        entry[24..32]
            .try_into()
            .map_err(|_| format!("decode row-change id for entry {sequence}"))?,
    );
    let successor_doc_id = u64::from_be_bytes(
        entry[32..40]
            .try_into()
            .map_err(|_| format!("decode row-change successor for entry {sequence}"))?,
    );
    let successor_table_hash = u64::from_be_bytes(
        entry[40..48]
            .try_into()
            .map_err(|_| format!("decode row-change successor table for entry {sequence}"))?,
    );
    let kind = match entry[4] {
        1 => PublishedRowChangeKind::Update,
        2 => PublishedRowChangeKind::Delete,
        3 => PublishedRowChangeKind::Rewrite(PublishedRowIdentity {
            // Journals created before cross-partition successor tracking left these reserved bytes zeroed; such rewrites were necessarily within the source table.
            table_hash: if successor_table_hash == 0 {
                table_hash
            } else {
                successor_table_hash
            },
            doc_id: successor_doc_id,
        }),
        kind => {
            return Err(format!(
                "row-change journal entry {sequence} has invalid kind {kind}"
            ));
        }
    };
    Ok(PublishedRowChange {
        table_hash,
        doc_id,
        kind,
        strength: decode_strength(entry[5]).ok_or_else(|| {
            format!(
                "row-change journal entry {sequence} has invalid lock strength {}",
                entry[5]
            )
        })?,
    })
}

const fn strength_code(strength: LockStrength) -> u8 {
    match strength {
        LockStrength::ForKeyShare => 0,
        LockStrength::ForShare => 1,
        LockStrength::ForNoKeyUpdate => 2,
        LockStrength::ForUpdate => 3,
    }
}

const fn decode_strength(code: u8) -> Option<LockStrength> {
    match code {
        0 => Some(LockStrength::ForKeyShare),
        1 => Some(LockStrength::ForShare),
        2 => Some(LockStrength::ForNoKeyUpdate),
        3 => Some(LockStrength::ForUpdate),
        _ => None,
    }
}
