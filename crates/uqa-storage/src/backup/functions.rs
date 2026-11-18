//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical source decisions for stored SQL calls. Their complete encoded unit is covered by the ordinary inventory digest.

use super::{
    BackupError, BackupResult, BackupSourceStatistics, BackupSourceStep, NamespaceId, UnitAddress,
    UnitKind,
};
use crate::read_control::StorageReadControl;

mod decoder;
mod digest;
mod order;
mod source;
pub(super) use decoder::FunctionDecoder;
pub use digest::BackupFunctionDigestJob;

const MAGIC: &[u8; 8] = b"UQAFUN01";
const END: u8 = 0;

/// The six declarations encode the presence or absence of an individual callable kind at a referenced name. SQL owns name normalization and selects the declarations that affect the stored call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BackupFunctionKind {
    Scalar = 1,
    Table = 2,
    Aggregate = 3,
    AbsentScalar = 4,
    AbsentTable = 5,
    AbsentAggregate = 6,
}

impl BackupFunctionKind {
    fn decode(tag: u8) -> BackupResult<Self> {
        match tag {
            1 => Ok(Self::Scalar),
            2 => Ok(Self::Table),
            3 => Ok(Self::Aggregate),
            4 => Ok(Self::AbsentScalar),
            5 => Ok(Self::AbsentTable),
            6 => Ok(Self::AbsentAggregate),
            _ => Err(BackupError::Unsupported(
                "registered SQL function declaration kind",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupFunctionReference<'a> {
    pub name: &'a str,
    pub kind: BackupFunctionKind,
}

/// One source selection per Unit. The current declaration remains borrowed until the next advance. Selections must be strictly ordered by (UTF-8 name, kind).
pub trait BackupFunctionSource: Send {
    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep>;
    fn current(&self) -> Option<BackupFunctionReference<'_>>;
    fn statistics(&self) -> BackupSourceStatistics {
        BackupSourceStatistics::default()
    }
}

fn address() -> UnitAddress<'static> {
    // This is the smallest nonzero namespace, before every provider mapping.
    UnitAddress {
        namespace: NamespaceId::from_bytes(1_u128.to_be_bytes()).expect("nonzero namespace"),
        generation: 1,
        family: 1,
        key: &[],
    }
}

pub(super) fn validate_position(unit: &super::InventoryEntry, ordinal: u64) -> BackupResult<()> {
    if unit.kind() == UnitKind::FunctionReferences && (ordinal != 0 || unit.address() != address())
    {
        return Err(BackupError::Invalid(
            "registered SQL function section position",
        ));
    }
    Ok(())
}

fn prefix(reference: BackupFunctionReference<'_>) -> BackupResult<[u8; 5]> {
    reference
        .name
        .len()
        .checked_add(5)
        .ok_or(BackupError::Invalid(
            "registered SQL function record length",
        ))?;
    let length = u32::try_from(reference.name.len())
        .map_err(|_| BackupError::Invalid("registered SQL function name length"))?;
    if length == 0 {
        return Err(BackupError::Invalid("empty registered SQL function name"));
    }
    let mut bytes = [reference.kind as u8; 5];
    bytes[1..].copy_from_slice(&length.to_le_bytes());
    Ok(bytes)
}
