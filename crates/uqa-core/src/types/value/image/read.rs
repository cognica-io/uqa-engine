//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{check_depth, ValueImageError, ValueImageResult};
use crate::image_io::AsyncImageInput;
use crate::{
    memory::{
        Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation, ProductionControl,
    },
    BitStringValue, CancellationToken, DecimalValue, EnumLabelKey, EnumValue, Value,
};

mod containers;
mod datum;
mod temporal;

enum Node {
    Value(Budgeted<Value>),
    Container(containers::Container),
}

pub(super) struct Reader<'a, 'b> {
    pub input: &'a mut AsyncImageInput<'b>,
    pub memory: &'b MemoryBudget,
    pub cancellation: &'b CancellationToken,
}

impl Reader<'_, '_> {
    pub(super) async fn fixed<const N: usize>(&mut self) -> ValueImageResult<[u8; N]> {
        self.input.array().await.map_err(Into::into)
    }

    async fn byte(&mut self) -> ValueImageResult<u8> {
        Ok(self.fixed::<1>().await?[0])
    }

    async fn flag(&mut self) -> ValueImageResult<bool> {
        match self.byte().await? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ValueImageError::Invalid("boolean flag")),
        }
    }

    async fn length(&mut self) -> ValueImageResult<usize> {
        usize::try_from(u64::from_le_bytes(self.fixed().await?))
            .map_err(|_| MemoryError::SizeOverflow.into())
    }

    async fn optional_oid(&mut self) -> ValueImageResult<Option<u32>> {
        if self.flag().await? {
            Ok(Some(u32::from_le_bytes(self.fixed().await?)))
        } else {
            Ok(None)
        }
    }

    async fn blob(&mut self) -> ValueImageResult<Budgeted<Vec<u8>>> {
        self.input.bytes().await.map_err(Into::into)
    }

    async fn text(&mut self) -> ValueImageResult<Budgeted<String>> {
        self.input.text().await.map_err(|error| match error {
            crate::image_io::ImageIoError::Invalid => ValueImageError::Invalid("UTF-8"),
            error => error.into(),
        })
    }

    fn scalar(&self, value: Value) -> Budgeted<Value> {
        Budgeted::new(value, self.memory.empty_reservation())
    }

    fn owned<T>(value: Budgeted<T>, wrap: impl FnOnce(T) -> Value) -> Budgeted<Value> {
        let (value, memory) = value.into_parts();
        Budgeted::new(wrap(value), memory)
    }

    pub(super) async fn value(&mut self, depth: usize) -> ValueImageResult<Budgeted<Value>> {
        let mut parents = BudgetedVec::new(self.memory);
        let mut node = self.node(depth).await?;
        loop {
            self.input.checkpoint().await?;
            match node {
                Node::Container(mut parent) => {
                    if parent.remaining == 0 {
                        node = Node::Value(parent.finish(self).await?);
                    } else {
                        parent.prepare(self).await?;
                        parents.push(parent)?;
                        node = self.node(depth + parents.len()).await?;
                    }
                }
                Node::Value(value) => {
                    let Some(mut parent) = parents.pop() else {
                        return Ok(value);
                    };
                    parent.accept(value)?;
                    node = Node::Container(parent);
                }
            }
        }
    }

    async fn node(&mut self, depth: usize) -> ValueImageResult<Node> {
        self.cancellation.check()?;
        check_depth(depth)?;
        let tag = self.byte().await?;
        let value = match tag {
            0 => self.scalar(Value::Null),
            1 => self.scalar(Value::Void),
            2 => {
                let value = self.flag().await?;
                self.scalar(Value::Bool(value))
            }
            3 => {
                let value = i64::from_le_bytes(self.fixed().await?);
                self.scalar(Value::Int(value))
            }
            4 => {
                let value = f64::from_bits(u64::from_le_bytes(self.fixed().await?));
                self.scalar(Value::Float(value))
            }
            5 => Self::owned(self.text().await?, Value::Str),
            6 => Self::owned(self.text().await?, Value::FixedChar),
            7 => Self::owned(self.blob().await?, Value::Bytes),
            8 => {
                let value = self.temporal().await?;
                self.scalar(Value::Temporal(value))
            }
            9 => {
                let text = self.text().await?;
                let value = DecimalValue::parse_budgeted(&text, self.memory, self.cancellation)?
                    .ok_or(ValueImageError::Invalid("decimal"))?;
                Self::owned(value, Value::Decimal)
            }
            10 => Self::owned(self.text().await?, Value::Json),
            11 => Self::owned(self.text().await?, Value::JsonB),
            12..=17 => {
                return Ok(Node::Container(
                    containers::Container::open(self, tag).await?,
                ))
            }
            18 => self.enum_value().await?,
            19 => self.datum().await?,
            20 => {
                let length = u32::from_le_bytes(self.fixed().await?);
                let bytes = self.blob().await?;
                let mut memory = self
                    .memory
                    .reserve(BitStringValue::retained_header_bytes())?;
                let (bytes, byte_memory) = bytes.into_parts();
                memory.absorb(byte_memory);
                let value = BitStringValue::try_from_packed(bytes, length)
                    .ok_or(ValueImageError::Invalid("packed bits"))?;
                Budgeted::new(Value::BitString(value), memory)
            }
            _ => return Err(ValueImageError::Invalid("value tag")),
        };
        self.cancellation.check()?;
        Ok(Node::Value(value))
    }

    async fn enum_value(&mut self) -> ValueImageResult<Budgeted<Value>> {
        let oid = u32::from_le_bytes(self.fixed().await?);
        let label = self.optional_oid().await?;
        let key = self.blob().await?;
        let mut memory = self.memory.reserve(EnumValue::retained_header_bytes())?;
        let source_bytes = key.capacity();
        // Converting a Vec to the compact key may allocate while the source buffer is live.
        memory.grow(key.len())?;
        let (key, key_memory) = key.into_parts();
        memory.absorb(key_memory);
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
