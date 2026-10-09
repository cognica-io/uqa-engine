//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode array contents with the element OID in the physical header, independently of the current attribute declaration.

use super::{
    corrupt, fixed, read_payload, word, Produced, ProductionControl, ProductionVec, SQLError, Value,
};
use crate::catalog::type_metadata::{
    builtin_scalar_type, pg_type_align, pg_type_by_value, pg_type_len,
};
use uqa_core::ArrayValue;

pub(crate) struct ArrayShape {
    dimensions: [usize; 6],
    bounds: [i32; 6],
    count: usize,
}

impl ArrayShape {
    pub(super) fn read(bytes: &[u8]) -> Result<Self, SQLError> {
        let count = integer(bytes, 0)?;
        if !(0..=6).contains(&count) {
            return Err(corrupt("invalid array dimensions"));
        }
        let count = count as usize;
        let mut shape = Self {
            dimensions: [0; 6],
            bounds: [0; 6],
            count,
        };
        for index in 0..count {
            shape.dimensions[index] = usize::try_from(integer(bytes, 12 + index * 4)?)
                .map_err(|_| corrupt("invalid array dimensions"))?;
            shape.bounds[index] = integer(bytes, 12 + count * 4 + index * 4)?;
        }
        Ok(shape)
    }

    pub(crate) fn dimensions(&self) -> &[usize] {
        &self.dimensions[..self.count]
    }
    pub(crate) fn lower_bounds(&self) -> &[i32] {
        &self.bounds[..self.count]
    }
}

pub(super) fn read(
    bytes: &[u8],
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let shape = ArrayShape::read(bytes)?;
    let dimensions = shape.dimensions();
    let ndim = dimensions.len();
    let dataoffset = integer(bytes, 4)?;
    let oid = integer(bytes, 8)? as u32;
    let resolved;
    let element = if let Some(element) = builtin_scalar_type(oid) {
        element
    } else {
        resolved = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("cache lookup failed for type {oid}"),
            })?
            .retain_external_with_control(control)?;
        &resolved
    };
    let mut bounds = ProductionVec::new(*control);
    let mut count = usize::from(ndim != 0);
    for (dimension, bound) in dimensions.iter().zip(shape.lower_bounds()) {
        count = count
            .checked_mul(*dimension)
            .ok_or_else(|| corrupt("invalid array dimensions"))?;
        bounds.push_copy(*bound)?;
    }
    let bitmap_start = 12 + ndim * 8;
    let bitmap = if dataoffset == 0 {
        None
    } else {
        Some(
            bytes
                .get(
                    bitmap_start
                        ..bitmap_start
                            .checked_add(count.div_ceil(8))
                            .ok_or_else(|| corrupt("invalid array dimensions"))?,
                )
                .ok_or_else(|| corrupt("invalid array null bitmap"))?,
        )
    };
    let offset = if dataoffset == 0 {
        align(bitmap_start + 4, b'd')?
    } else {
        usize::try_from(dataoffset).map_err(|_| corrupt("invalid array data offset"))?
    }
    .checked_sub(4)
    .filter(|offset| *offset >= bitmap_start && *offset <= bytes.len())
    .ok_or_else(|| corrupt("invalid array data offset"))?;
    let mut reader = Elements {
        bytes,
        bitmap,
        index: 0,
        offset,
        element,
        catalog,
        control: *control,
    };
    let values = reader.dimension(dimensions)?;
    let array = ArrayValue::with_lower_bounds_with_control(values, bounds.finish()?, control)?
        .ok_or_else(|| corrupt("invalid array dimensions"))?;
    let (array, memory) = array.into_parts();
    Ok(control.finish(Value::Array(array.with_element_type_oid(Some(oid))), memory)?)
}

struct Elements<'a, 'c> {
    bytes: &'a [u8],
    bitmap: Option<&'a [u8]>,
    index: usize,
    offset: usize,
    element: &'a crate::ColumnType,
    catalog: Option<&'a dyn crate::expr::SQLValueCatalog>,
    control: ProductionControl<'c>,
}

impl Elements<'_, '_> {
    fn dimension(&mut self, dimensions: &[usize]) -> Result<Produced<Vec<Value>>, SQLError> {
        let mut values = ProductionVec::new(self.control);
        let Some((length, nested)) = dimensions.split_first() else {
            return Ok(values.finish()?);
        };
        values.reserve(*length)?;
        for _ in 0..*length {
            self.control.check()?;
            let value = if nested.is_empty() {
                self.element()?
            } else {
                let (values, memory) = self.dimension(nested)?.into_parts();
                self.control.finish(Value::List(values), memory)?
            };
            values.push_produced(value)?;
        }
        Ok(values.finish()?)
    }

    fn element(&mut self) -> Result<Produced<Value>, SQLError> {
        let null = self
            .bitmap
            .is_some_and(|bitmap| bitmap[self.index / 8] & (1 << (self.index % 8)) == 0);
        self.index += 1;
        if null {
            return Ok(self
                .control
                .finish(Value::Null, self.control.empty_reservation())?);
        }
        let alignment = pg_type_align(self.element).as_bytes()[0];
        self.offset = align(
            self.offset
                .checked_add(4)
                .ok_or_else(|| corrupt("invalid array length"))?,
            alignment,
        )? - 4;
        let remaining = self
            .bytes
            .get(self.offset..)
            .ok_or_else(|| corrupt("invalid array length"))?;
        let length = pg_type_len(self.element);
        let value = if length == -1 {
            let header = word(remaining).ok_or_else(|| corrupt("invalid array element length"))?;
            let length = (header >> 2) as usize;
            let payload = remaining
                .get(4..length)
                .filter(|_| header.trailing_zeros() >= 2)
                .ok_or_else(|| corrupt("invalid array element length"))?;
            self.offset += length;
            read_payload(base_oid(self.element), payload, self.catalog, &self.control)?
        } else {
            let length =
                usize::try_from(length).map_err(|_| corrupt("invalid array element length"))?;
            let bytes = remaining
                .get(..length)
                .ok_or_else(|| corrupt("invalid array element length"))?;
            self.offset += length;
            if pg_type_by_value(self.element) && length <= 8 {
                let mut bits = [0; 8];
                bits[..length].copy_from_slice(bytes);
                let value = crate::expr::composites::datum::decode_bits(
                    u64::from_le_bytes(bits),
                    self.element,
                )
                .ok_or_else(|| corrupt("invalid array element datum"))?;
                self.control.copy_value(&value)?
            } else {
                fixed::read_bytes(base_oid(self.element), bytes, &self.control)?
            }
        };
        Ok(value)
    }
}

fn integer(bytes: &[u8], offset: usize) -> Result<i32, SQLError> {
    word(bytes.get(offset..).unwrap_or_default())
        .map(|value| value as i32)
        .ok_or_else(|| corrupt("invalid array header"))
}

pub(super) fn align(offset: usize, alignment: u8) -> Result<usize, SQLError> {
    let mask = match alignment {
        b'c' => 0,
        b's' => 1,
        b'i' => 3,
        b'd' => 7,
        _ => return Err(corrupt("invalid array element alignment")),
    };
    offset
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| corrupt("invalid array length"))
}

fn base_oid(mut ty: &crate::ColumnType) -> u32 {
    while let crate::ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    crate::catalog::type_metadata::pg_type_oid(ty) as u32
}
