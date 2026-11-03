//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Budgeted, BudgetedVec, Reader, Value, ValueImageResult};
use crate::{DatumValue, RecordDatumField};

impl Reader<'_> {
    pub(super) fn datum(&mut self) -> ValueImageResult<Budgeted<Value>> {
        let oid = u32::from_le_bytes(self.fixed()?);
        let offset = u32::from_le_bytes(self.fixed()?);
        let (bytes, mut memory) = self.blob()?.into_parts();
        memory.grow(DatumValue::retained_header_bytes())?;
        let mut datum = DatumValue::new(oid, offset, bytes);
        if self.flag()? {
            let count = self.length()?;
            let mut fields = BudgetedVec::new(self.memory);
            fields.reserve(count)?;
            for _ in 0..count {
                let (name, name_memory) = self.text()?.into_parts();
                memory.absorb(name_memory);
                fields.push(RecordDatumField {
                    name,
                    type_oid: u32::from_le_bytes(self.fixed()?),
                    type_modifier: i32::from_le_bytes(self.fixed()?),
                    collation_oid: self.optional_oid()?,
                    length: i64::from_le_bytes(self.fixed()?),
                    alignment: self.byte()?,
                    dropped: self.flag()?,
                })?;
            }
            memory.grow(DatumValue::retained_header_bytes())?;
            let (fields, field_memory) = fields.into_parts();
            memory.absorb(field_memory);
            datum = datum.with_record_fields(fields);
        }
        Ok(Budgeted::new(Value::Datum(datum), memory))
    }
}
