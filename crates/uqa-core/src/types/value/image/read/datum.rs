//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Budgeted, BudgetedVec, Reader, Value, ValueImageResult};
use crate::{DatumValue, RecordDatumField};

impl Reader<'_, '_> {
    pub(super) async fn datum(&mut self) -> ValueImageResult<Budgeted<Value>> {
        let oid = u32::from_le_bytes(self.fixed().await?);
        let offset = u32::from_le_bytes(self.fixed().await?);
        let bytes = self.blob().await?;
        let mut memory = self.memory.reserve(DatumValue::retained_header_bytes())?;
        let (bytes, byte_memory) = bytes.into_parts();
        memory.absorb(byte_memory);
        let mut datum = DatumValue::new(oid, offset, bytes);
        if self.flag().await? {
            let count = self.length().await?;
            let mut fields = BudgetedVec::new(self.memory);
            fields.reserve(count)?;
            for _ in 0..count {
                let (name, name_memory) = self.text().await?.into_parts();
                memory.absorb(name_memory);
                fields.push(RecordDatumField {
                    name,
                    type_oid: u32::from_le_bytes(self.fixed().await?),
                    type_modifier: i32::from_le_bytes(self.fixed().await?),
                    collation_oid: self.optional_oid().await?,
                    length: i64::from_le_bytes(self.fixed().await?),
                    alignment: self.byte().await?,
                    dropped: self.flag().await?,
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
