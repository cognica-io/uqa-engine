//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{check_depth, ValueImageError, ValueImageResult, WINDOW};
use crate::{
    memory::{
        Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation, ProductionControl,
    },
    BitStringValue, CancellationToken, DecimalValue, EnumLabelKey, EnumValue, Value,
};
use std::io::Read;

mod containers;
mod datum;
mod temporal;

pub(super) struct Reader<'a> {
    pub input: &'a mut dyn Read,
    pub memory: &'a MemoryBudget,
    pub cancellation: &'a CancellationToken,
}

impl Reader<'_> {
    fn exact(&mut self, mut bytes: &mut [u8]) -> ValueImageResult<()> {
        while !bytes.is_empty() {
            self.cancellation.check()?;
            match self.input.read(bytes) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()),
                Ok(count) if count <= bytes.len() => bytes = &mut bytes[count..],
                Ok(_) => return Err(ValueImageError::Invalid("reader byte count")),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.cancellation.check()?;
        Ok(())
    }

    pub(super) fn fixed<const N: usize>(&mut self) -> ValueImageResult<[u8; N]> {
        self.cancellation.check()?;
        let mut bytes = [0; N];
        self.exact(&mut bytes)?;
        self.cancellation.check()?;
        Ok(bytes)
    }

    fn byte(&mut self) -> ValueImageResult<u8> {
        Ok(self.fixed::<1>()?[0])
    }

    fn flag(&mut self) -> ValueImageResult<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ValueImageError::Invalid("boolean flag")),
        }
    }

    fn length(&mut self) -> ValueImageResult<usize> {
        usize::try_from(u64::from_le_bytes(self.fixed()?))
            .map_err(|_| MemoryError::SizeOverflow.into())
    }

    fn optional_oid(&mut self) -> ValueImageResult<Option<u32>> {
        if self.flag()? {
            Ok(Some(u32::from_le_bytes(self.fixed()?)))
        } else {
            Ok(None)
        }
    }

    fn blob(&mut self) -> ValueImageResult<Budgeted<Vec<u8>>> {
        let size = self.length()?;
        let mut bytes = BudgetedVec::new(self.memory);
        bytes.reserve(size)?;
        let mut buffer = [0; WINDOW];
        while bytes.len() < size {
            self.cancellation.check()?;
            let count = (size - bytes.len()).min(buffer.len());
            self.exact(&mut buffer[..count])?;
            bytes.extend_from_slice(&buffer[..count])?;
        }
        self.cancellation.check()?;
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(bytes, memory))
    }

    fn text(&mut self) -> ValueImageResult<Budgeted<String>> {
        let (bytes, memory) = self.blob()?.into_parts();
        let text = String::from_utf8(bytes).map_err(|_| ValueImageError::Invalid("UTF-8"))?;
        Ok(Budgeted::new(text, memory))
    }

    fn values(&mut self, depth: usize) -> ValueImageResult<Budgeted<Vec<Value>>> {
        let count = self.length()?;
        let mut payload = self.memory.empty_reservation();
        let mut values = BudgetedVec::new(self.memory);
        values.reserve(count)?;
        for _ in 0..count {
            let (value, memory) = self.value(depth + 1)?.into_parts();
            values.push(value)?;
            payload.absorb(memory);
        }
        let (values, mut memory) = values.into_parts();
        memory.absorb(payload);
        Ok(Budgeted::new(values, memory))
    }

    fn scalar(&self, value: Value) -> Budgeted<Value> {
        Budgeted::new(value, self.memory.empty_reservation())
    }

    fn owned<T>(value: Budgeted<T>, wrap: impl FnOnce(T) -> Value) -> Budgeted<Value> {
        let (value, memory) = value.into_parts();
        Budgeted::new(wrap(value), memory)
    }

    pub(super) fn value(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        self.cancellation.check()?;
        check_depth(depth)?;
        let tag = self.byte()?;
        let value = match tag {
            0 => self.scalar(Value::Null),
            1 => self.scalar(Value::Void),
            2 => {
                let value = self.flag()?;
                self.scalar(Value::Bool(value))
            }
            3 => {
                let value = i64::from_le_bytes(self.fixed()?);
                self.scalar(Value::Int(value))
            }
            4 => {
                let value = f64::from_bits(u64::from_le_bytes(self.fixed()?));
                self.scalar(Value::Float(value))
            }
            5 => Self::owned(self.text()?, Value::Str),
            6 => Self::owned(self.text()?, Value::FixedChar),
            7 => Self::owned(self.blob()?, Value::Bytes),
            8 => {
                let value = self.temporal()?;
                self.scalar(Value::Temporal(value))
            }
            9 => {
                let text = self.text()?;
                let value = DecimalValue::parse_budgeted(&text, self.memory, self.cancellation)?
                    .ok_or(ValueImageError::Invalid("decimal"))?;
                Self::owned(value, Value::Decimal)
            }
            10 => Self::owned(self.text()?, Value::Json),
            11 => Self::owned(self.text()?, Value::JsonB),
            12 => Self::owned(self.array(depth)?, Value::Array),
            13 => self.legacy_vector(depth)?,
            14 => Self::owned(self.values(depth)?, Value::List),
            15 => self.row(depth)?,
            16 => self.record(depth)?,
            17 => self.map(depth)?,
            18 => self.enum_value()?,
            19 => self.datum()?,
            20 => {
                let length = u32::from_le_bytes(self.fixed()?);
                let (bytes, mut memory) = self.blob()?.into_parts();
                memory.grow(BitStringValue::retained_header_bytes())?;
                let value = BitStringValue::try_from_packed(bytes, length)
                    .ok_or(ValueImageError::Invalid("packed bits"))?;
                Budgeted::new(Value::BitString(value), memory)
            }
            _ => return Err(ValueImageError::Invalid("value tag")),
        };
        self.cancellation.check()?;
        Ok(value)
    }

    fn enum_value(&mut self) -> ValueImageResult<Budgeted<Value>> {
        let oid = u32::from_le_bytes(self.fixed()?);
        let label = self.optional_oid()?;
        let (key, mut memory) = self.blob()?.into_parts();
        let source_bytes = key.capacity();
        // Converting a Vec to the compact key may allocate while the source buffer is live.
        memory.grow(key.len())?;
        memory.grow(EnumValue::retained_header_bytes())?;
        let key =
            EnumLabelKey::from_bytes(key).map_err(|_| ValueImageError::Invalid("enum key"))?;
        drop(memory.split(source_bytes));
        Ok(Budgeted::new(
            Value::Enum(EnumValue::new(oid, key).with_label_oid(label)),
            memory,
        ))
    }

    fn control(&self) -> ProductionControl<'_> {
        ProductionControl::new(self.memory, self.cancellation, self.cancellation)
    }

    fn entries<T>(&self, count: usize) -> ValueImageResult<MemoryReservation> {
        Ok(self.memory.reserve(
            count
                .checked_mul(size_of::<T>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?)
    }
}
