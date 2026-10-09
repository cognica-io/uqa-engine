//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite array elements retain their own tuple type and defer scalar field observation.

use super::{
    corrupt, word, DatumValue, Produced, ProductionControl, ProductionVec, SQLError, Value,
};
use crate::catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid};
use crate::expr::SQLValueCatalog;
use crate::ColumnType;

pub(super) fn read(
    bytes: &[u8],
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let type_oid =
        word(bytes.get(4..).unwrap_or_default()).ok_or_else(|| corrupt("invalid record header"))?;
    let descriptor = catalog
        .map(|catalog| catalog.value_composite_type(type_oid))
        .transpose()?
        .flatten()
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {type_oid} does not exist"),
        })?;
    let RecordLayout {
        count,
        mut offset,
        bitmap,
    } = RecordLayout::read(bytes)?;

    let mut backing = ProductionVec::new(*control);
    backing.reserve(
        bytes
            .len()
            .checked_add(4)
            .ok_or_else(|| corrupt("invalid record length"))?,
    )?;
    let header = u32::try_from(bytes.len())
        .ok()
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| corrupt("invalid record length"))?;
    for byte in header.to_le_bytes().iter().chain(bytes) {
        backing.push_copy(*byte)?;
    }
    let (backing, backing_memory) = backing.finish()?.into_parts();
    let backing = DatumValue::new(type_oid, 0, backing);
    let mut fields = ProductionVec::new(*control);
    fields.reserve(descriptor.attributes.len())?;
    let mut live = descriptor.attributes.iter().peekable();
    for number in 1..=count {
        control.check()?;
        let attribute = live
            .peek()
            .filter(|attribute| usize::try_from(attribute.number).ok() == Some(number))
            .copied();
        let dropped = descriptor
            .dropped
            .iter()
            .find(|attribute| usize::try_from(attribute.number).ok() == Some(number));
        let is_null =
            bitmap.is_some_and(|bitmap| bitmap[(number - 1) / 8] & (1 << ((number - 1) % 8)) == 0);
        let mut value = control.finish(Value::Null, control.empty_reservation())?;
        if !is_null {
            let (length, alignment) = if let Some(attribute) = attribute {
                (
                    pg_type_len(&attribute.ty),
                    pg_type_align(&attribute.ty).as_bytes()[0],
                )
            } else if let Some(dropped) = dropped {
                (dropped.length, dropped.alignment)
            } else {
                return Err(corrupt("record descriptor is missing a physical attribute"));
            };
            if length != -1 || backing.bytes().get(offset) == Some(&0) {
                offset = super::arrays::align(offset, alignment)?;
            }
            if let Some(attribute) = attribute {
                let mut ty = &attribute.ty;
                while let ColumnType::Domain { base, .. } = ty {
                    ty = base;
                }
                value = control.copy_value(&Value::Datum(backing.field(
                    pg_type_oid(ty) as u32,
                    u32::try_from(offset).map_err(|_| corrupt("invalid record length"))?,
                )))?;
            }
            let length = if length == -1 {
                stored_length(backing.bytes().get(offset..).unwrap_or_default())?
            } else {
                usize::try_from(length).map_err(|_| corrupt("invalid record attribute length"))?
            };
            offset = offset
                .checked_add(length)
                .ok_or_else(|| corrupt("invalid record length"))?;
        }
        if let Some(attribute) = attribute {
            push_field(&mut fields, &attribute.name, value, control)?;
            live.next();
        }
    }
    for attribute in live {
        push_field(
            &mut fields,
            &attribute.name,
            control.finish(Value::Null, control.empty_reservation())?,
            control,
        )?;
    }
    let (fields, memory) =
        uqa_core::RecordValue::with_control(fields.finish()?, Some(type_oid), control)?
            .into_parts();
    drop(backing_memory);
    Ok(control.finish(Value::Record(fields), memory)?)
}

struct RecordLayout<'a> {
    count: usize,
    offset: usize,
    bitmap: Option<&'a [u8]>,
}

impl<'a> RecordLayout<'a> {
    fn read(bytes: &'a [u8]) -> Result<Self, SQLError> {
        let count = usize::from(
            u16::from_le_bytes(
                bytes
                    .get(14..16)
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| corrupt("invalid record header"))?,
            ) & 0x07ff,
        );
        let mask = u16::from_le_bytes(
            bytes
                .get(16..18)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| corrupt("invalid record header"))?,
        );
        let offset = usize::from(
            *bytes
                .get(18)
                .ok_or_else(|| corrupt("invalid record header"))?,
        );
        if offset < 24 || offset > bytes.len() + 4 {
            return Err(corrupt("invalid record header"));
        }
        let bitmap = if mask & 1 != 0 {
            Some(
                bytes
                    .get(19..19 + count.div_ceil(8))
                    .ok_or_else(|| corrupt("invalid record null bitmap"))?,
            )
        } else {
            None
        };
        Ok(Self {
            count,
            offset,
            bitmap,
        })
    }
}

fn push_field(
    fields: &mut ProductionVec<'_, (String, Value)>,
    name: &str,
    value: Produced<Value>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (name, name_memory) = control.copy_text(name)?.into_parts();
    let (value, memory) = value.into_parts();
    fields.push_produced(control.finish((name, value), control.combine(name_memory, memory))?)?;
    Ok(())
}

fn stored_length(bytes: &[u8]) -> Result<usize, SQLError> {
    let first = *bytes
        .first()
        .ok_or_else(|| corrupt("invalid record attribute length"))?;
    if first == 1 {
        return Ok(match bytes.get(1) {
            Some(1..=3) => 10,
            Some(18) => 18,
            _ => 2,
        });
    }
    if first & 1 != 0 {
        return Ok(usize::from(first >> 1));
    }
    word(bytes)
        .map(|header| (header >> 2) as usize)
        .ok_or_else(|| corrupt("invalid record attribute length"))
}
