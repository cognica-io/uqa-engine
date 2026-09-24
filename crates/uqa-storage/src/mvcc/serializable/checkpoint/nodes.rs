//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Participant ordering and physical outcome state have one checked checkpoint representation.

use super::super::ParticipantOwner;
use super::{
    io::{invalid, Decoder, Encoder},
    SerializableGraph, Transaction, VersionResult,
};
use crate::mvcc::{
    serializable::publication::{PreparedPublication, PublicationOutcome},
    CommitSequence,
};

pub(super) fn write(encoder: &mut Encoder<'_>, entry: Transaction) -> VersionResult<()> {
    for value in [
        entry.id,
        entry.snapshot,
        entry.prepared.unwrap_or(0),
        entry.committed.unwrap_or(0),
        entry.summarized_out.unwrap_or(0),
        entry.writes,
    ] {
        encoder.number(value)?;
    }
    encoder.byte(
        u8::from(entry.read_only)
            | (u8::from(entry.aborted) << 1)
            | (u8::from(entry.doomed) << 2)
            | (u8::from(entry.owner == ParticipantOwner::Leased) << 3),
    )?;
    let Some(publication) = entry.publication else {
        return encoder.byte(0);
    };
    encoder.byte(match publication.outcome {
        PublicationOutcome::Prepared => 1,
        PublicationOutcome::Committed(_) => 2,
        PublicationOutcome::Aborted => 3,
    })?;
    encoder.number(publication.allocation)?;
    encoder.bytes(&publication.fingerprint)?;
    if let PublicationOutcome::Committed(sequence) = publication.outcome {
        encoder.number(sequence.as_u64())?;
    }
    Ok(())
}

pub(super) fn restore(
    graph: &mut SerializableGraph,
    count: usize,
    supports_leases: bool,
    supports_read_only_publication: bool,
    decoder: &mut Decoder<'_>,
) -> VersionResult<()> {
    graph.transactions.reserve(count)?;
    let mut pending = 0;
    for _ in 0..count {
        let entry = read(
            decoder,
            graph.clock,
            graph.last_allocation,
            supports_leases,
            supports_read_only_publication,
        )?;
        if graph
            .transactions
            .last()
            .is_some_and(|previous| previous.id >= entry.id)
        {
            return Err(invalid());
        }
        pending += u64::from(entry.prepared.is_some() && entry.live());
        graph.transactions.push(entry)?;
    }
    if pending != graph.pending_finishes {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn read(
    decoder: &mut Decoder<'_>,
    clock: u64,
    allocation: u64,
    supports_leases: bool,
    supports_read_only_publication: bool,
) -> VersionResult<Transaction> {
    let id = decoder.number()?;
    let snapshot = decoder.number()?;
    let prepared = nonzero(decoder.number()?);
    let committed = nonzero(decoder.number()?);
    let summarized_out = nonzero(decoder.number()?);
    let writes = decoder.number()?;
    let flags = decoder.byte()?;
    if flags & !(if supports_leases { 15 } else { 7 }) != 0
        || id == 0
        || id > allocation
        || snapshot == 0
        || snapshot > clock
        || prepared.is_some_and(|order| order <= snapshot || order > clock)
        || committed
            .is_some_and(|order| prepared.is_none_or(|start| order <= start) || order > clock)
        || summarized_out.is_some_and(|order| order > clock)
    {
        return Err(invalid());
    }
    let mut entry = Transaction {
        id,
        snapshot,
        prepared,
        committed,
        summarized_out,
        writes,
        read_only: flags & 1 != 0,
        aborted: flags & 2 != 0,
        doomed: flags & 4 != 0,
        owner: if flags & 8 != 0 {
            ParticipantOwner::Leased
        } else {
            ParticipantOwner::Manual
        },
        publication: None,
    };
    if (entry.aborted && committed.is_some())
        || (entry.doomed && prepared.is_some())
        || (entry.read_only && writes != 0)
    {
        return Err(invalid());
    }
    let tag = decoder.byte()?;
    if tag == 0 {
        return Ok(entry);
    }
    if tag > 3 || (entry.read_only && !supports_read_only_publication) || entry.prepared.is_none() {
        return Err(invalid());
    }
    let allocation = decoder.number()?;
    let fingerprint = decoder.array()?;
    if allocation == 0 {
        return Err(invalid());
    }
    let outcome = match tag {
        1 if entry.live() && !entry.doomed => PublicationOutcome::Prepared,
        2 if entry.committed.is_some() => {
            // A durable empty commit keeps the current sequence, including INITIAL. The publication tag distinguishes a committed receipt from an unresolved outcome.
            PublicationOutcome::Committed(CommitSequence::from_u64(decoder.number()?))
        }
        3 if entry.aborted => PublicationOutcome::Aborted,
        _ => return Err(invalid()),
    };
    entry.publication = Some(PreparedPublication {
        allocation,
        fingerprint,
        outcome,
    });
    Ok(entry)
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}
