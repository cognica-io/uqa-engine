//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode array contents with the element OID in the physical header, independently of the current attribute declaration.

use super::{corrupt, inline, word, Produced, ProductionControl, ProductionVec, SQLError, Value};
use crate::catalog::type_metadata::builtin_scalar_type;
use uqa_core::ArrayValue;

/// Dimensions and lower bounds read from an admitted physical array header.
pub struct ArrayShape {
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

    pub fn dimensions(&self) -> &[usize] {
        &self.dimensions[..self.count]
    }
    pub fn lower_bounds(&self) -> &[i32] {
        &self.bounds[..self.count]
    }
}

pub(super) fn read(
    bytes: &[u8],
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let (array, memory) = read_array(bytes, catalog, control)?.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

pub(super) fn read_legacy_vector(
    bytes: &[u8],
    kind: uqa_core::LegacyVectorKind,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let element = match kind {
        uqa_core::LegacyVectorKind::SmallInteger => &crate::ColumnType::SmallInteger,
        uqa_core::LegacyVectorKind::Oid => &crate::ColumnType::Oid,
    };
    let oid = crate::catalog::type_metadata::pg_type_oid(element) as u32;
    let invalid = || SQLError::Routine {
        sqlstate: "42804".into(),
        message: format!("array is not a valid {}", kind.type_name()),
    };
    if ArrayShape::read(bytes)?.dimensions().len() > 1
        || integer(bytes, 4)? != 0
        || integer(bytes, 8)? as u32 != oid
    {
        return Err(invalid());
    }
    let array = read_array(bytes, None, control)?;
    let vector = uqa_core::LegacyVectorValue::try_from_array_with_control(kind, array, control)?
        .ok_or_else(invalid)?;
    let (vector, memory) = vector.into_parts();
    Ok(control.finish(Value::LegacyVector(vector), memory)?)
}

fn read_array(
    bytes: &[u8],
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ArrayValue>, SQLError> {
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
            .map(|catalog| catalog.value_type_by_oid_with_control(oid, control))
            .transpose()?
            .flatten()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("cache lookup failed for type {oid}"),
            })?;
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
        inline::align(bitmap_start + 4, b'd')?
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
    Ok(control.finish(array.with_element_type_oid(Some(oid)), memory)?)
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
        inline::read(
            self.bytes,
            &mut self.offset,
            4,
            self.element,
            self.catalog,
            &self.control,
        )
    }
}

fn integer(bytes: &[u8], offset: usize) -> Result<i32, SQLError> {
    word(bytes.get(offset..).unwrap_or_default())
        .map(|value| value as i32)
        .ok_or_else(|| corrupt("invalid array header"))
}
