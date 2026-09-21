//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate checkpoint keys remain owned, budgeted and independent of physical provider indexes.

use std::ops::Bound;

use uqa_core::memory::BudgetedVec;

use super::{
    io::{invalid, Decoder, Encoder},
    VersionResult,
};
use crate::mvcc::serializable::{
    observations::Observation,
    predicates::{OwnedPredicate, SerializableKeySpace},
};

pub(super) fn write(encoder: &mut Encoder<'_>, observation: &Observation) -> VersionResult<()> {
    encoder.number(observation.owner)?;
    encoder.number(observation.write)?;
    let predicate = &observation.predicate;
    encoder.bytes(&predicate.object)?;
    let Some(space) = predicate.space else {
        return encoder.byte(0);
    };
    encoder.byte(match (predicate.point, space) {
        (true, SerializableKeySpace::Rows) => 1,
        (true, SerializableKeySpace::Index(_)) => 2,
        (false, SerializableKeySpace::Rows) => 3,
        (false, SerializableKeySpace::Index(_)) => 4,
        (true, SerializableKeySpace::Vectors) => 5,
        (false, SerializableKeySpace::Vectors) => 6,
    })?;
    if let SerializableKeySpace::Index(identity) = space {
        encoder.bytes(&identity)?;
    }
    write_bound(encoder, &predicate.lower)?;
    if !predicate.point {
        write_bound(encoder, &predicate.upper)?;
    }
    Ok(())
}

pub(super) fn read(decoder: &mut Decoder<'_>, writing: bool) -> VersionResult<Observation> {
    let owner = decoder.number()?;
    let write = decoder.number()?;
    let object = decoder.array()?;
    let tag = decoder.byte()?;
    let space = match tag {
        0 => None,
        1 | 3 => Some(SerializableKeySpace::Rows),
        2 | 4 => Some(SerializableKeySpace::Index(decoder.array()?)),
        5 | 6 => Some(SerializableKeySpace::Vectors),
        _ => return Err(invalid()),
    };
    let point = matches!(tag, 1 | 2 | 5);
    let lower = if space.is_some() {
        read_bound(decoder)?
    } else {
        Bound::Unbounded
    };
    if point && !matches!(lower, Bound::Included(_)) {
        return Err(invalid());
    }
    let upper = if space.is_some() && !point {
        read_bound(decoder)?
    } else {
        Bound::Unbounded
    };
    let predicate = OwnedPredicate {
        object,
        space,
        point,
        lower,
        upper,
    };
    predicate.borrowed().validate(writing)?;
    if predicate.borrowed().is_empty() {
        return Err(invalid());
    }
    Ok(Observation {
        owner,
        write,
        predicate,
    })
}

fn write_bound(encoder: &mut Encoder<'_>, bound: &Bound<BudgetedVec<u8>>) -> VersionResult<()> {
    match bound {
        Bound::Unbounded => encoder.byte(0),
        Bound::Included(key) => {
            encoder.byte(1)?;
            encoder.key(key)
        }
        Bound::Excluded(key) => {
            encoder.byte(2)?;
            encoder.key(key)
        }
    }
}

fn read_bound(decoder: &mut Decoder<'_>) -> VersionResult<Bound<BudgetedVec<u8>>> {
    match decoder.byte()? {
        0 => Ok(Bound::Unbounded),
        1 => Ok(Bound::Included(decoder.key()?)),
        2 => Ok(Bound::Excluded(decoder.key()?)),
        _ => Err(invalid()),
    }
}
