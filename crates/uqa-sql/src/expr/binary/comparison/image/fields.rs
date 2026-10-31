//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed field images share selected record comparison's admitted SQL codec.

use crate::{
    catalog::type_metadata,
    expr::{datums, SQLRecordFields, SQLValueCatalog},
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// Visit each live field's selected identity and admitted image, with `None` for NULL. The callback borrows each image only while its controlled owner remains live. Execution owns framing and exact-key production; observation invokes no field operators or type input.
pub fn visit_record_image_fields_with_catalog(
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    mut visit: impl FnMut(Option<u32>, Option<&[u8]>) -> Result<(), SQLError>,
) -> Result<usize, SQLError> {
    control.check()?;
    let decoded = read(value, catalog, control)?;
    let fields = SQLRecordFields::new(decoded.as_deref().unwrap_or(value), catalog)?;
    let composites = super::super::support::ValueCompositeCatalog(catalog);
    let mut count = 0;
    while let Some(value) = fields.value(count) {
        control.check()?;
        let oid = fields.field_type_oid(count);
        if matches!(value, Value::Null) {
            visit(oid, None)?;
        } else {
            with_type(oid, catalog, control, |ty| {
                let image = datums::encoding::encode(value, ty, Some(&composites), control)?;
                visit(oid, Some(&image))
            })?;
        }
        count += 1;
    }
    control.check()?;
    Ok(count)
}

pub(super) fn read(
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    match value {
        Value::Datum(datum) => {
            datums::read_with_value_catalog_and_control(datum, catalog, control).map(Some)
        }
        _ => Ok(None),
    }
}

pub(super) fn with_type<T>(
    oid: Option<u32>,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    use_type: impl FnOnce(&ColumnType) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let oid = oid.ok_or_else(|| {
        SQLError::Internal("record image field has no selected SQL type identity".into())
    })?;
    if let Some(ty) = type_metadata::builtin_scalar_type(oid) {
        return use_type(ty);
    }
    let selected = catalog
        .map(|catalog| catalog.value_type_by_oid_with_control(oid, control))
        .transpose()?
        .flatten();
    let ty = selected.as_deref().ok_or_else(|| SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("type with OID {oid} does not exist"),
    })?;
    use_type(ty)
}
