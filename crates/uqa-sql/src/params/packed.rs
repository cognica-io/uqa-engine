//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host binary input retains SQL identity and uses the native representation owners.

use super::{ColumnType, Produced, ProductionControl, SQLError, SQLParam, Value};
use crate::expr::bit_string::{self, BitStringKind};

impl SQLParam {
    /// Construct a UUID from canonical network-order octets without applying version or variant restrictions.
    pub fn uuid_with_control(
        bytes: [u8; 16],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (text, memory) = crate::expr::format_uuid_with_control(bytes, control)?.into_parts();
        Ok(control.finish(
            Self::typed_scalar(Value::Str(text), ColumnType::Uuid),
            memory,
        )?)
    }

    /// Copy a packed fixed or varying bit value with no declared length modifier. Logical length and SQL kind remain independent.
    pub fn packed_bits_with_control(
        bytes: &[u8],
        bit_length: u32,
        kind: BitStringKind,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (bits, memory) =
            bit_string::from_packed_with_control(bytes, bit_length, control)?.into_parts();
        let ty = match kind {
            BitStringKind::Fixed => ColumnType::Bit(None),
            BitStringKind::Varying => ColumnType::VarBit(None),
        };
        Ok(control.finish(Self::typed_scalar(Value::BitString(bits), ty), memory)?)
    }
}

#[cfg(test)]
mod tests;
