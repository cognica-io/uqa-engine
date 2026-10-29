//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve declared column types through catalog type metadata.

use super::FunctionTypeResolver;
use crate::{ColumnType, SQLError};

pub fn resolve_declared_column_type(
    resolver: &dyn FunctionTypeResolver,
    ty: &ColumnType,
) -> Result<ColumnType, SQLError> {
    resolve_declared_column_type_with_control(
        resolver,
        ty,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary declared type owner".into()))
}

pub fn resolve_declared_column_type_with_control(
    resolver: &dyn FunctionTypeResolver,
    ty: &ColumnType,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<ColumnType>, SQLError> {
    control.check()?;
    if let ColumnType::Domain {
        schema,
        name,
        oid,
        array_oid,
        collation_oid,
        base,
    } = ty
    {
        let current = resolve_declared_column_type_with_control(resolver, base, control)?;
        let schema = control.copy_text(schema)?;
        let name = control.copy_text(name)?;
        let allocation = control.reserve(size_of::<ColumnType>())?;
        let (base, base_memory) = current.into_parts();
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        return Ok(control.finish(
            ColumnType::Domain {
                schema,
                name,
                oid: *oid,
                array_oid: *array_oid,
                collation_oid: *collation_oid,
                base: Box::new(base),
            },
            control.combine(
                control.combine(base_memory, allocation),
                control.combine(schema_memory, name_memory),
            ),
        )?);
    }
    let mut element = ty;
    let mut dimensions = 0;
    while let ColumnType::Array(inner) = element {
        control.check()?;
        element = inner;
        dimensions += 1;
    }
    if let ColumnType::Named(name) = element {
        let mut declaration = uqa_core::memory::ProductionString::new(*control);
        declaration.push_str(name)?;
        for _ in 0..dimensions {
            declaration.push_str("[]")?;
        }
        let declaration = declaration.finish()?;
        return resolver
            .resolve_type_name_with_control(&declaration, control)?
            .map_or_else(
                || ColumnType::from_sql_name_with_control(&declaration, control),
                Ok,
            );
    }
    if let ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) =
        element
    {
        let name = ty.catalog_name_with_control(control)?;
        let current = resolver
            .resolve_type_name_with_control(&name, control)?
            .ok_or_else(|| {
                match control.format(format_args!(
                    "type with OID {} does not exist",
                    reference.oid
                )) {
                    Ok(message) => SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: message.into_parts().0,
                    },
                    Err(error) => error.into(),
                }
            })?;
        if *current != *ty {
            return Err(SQLError::Internal(
                "bound range identity resolved to another type".into(),
            ));
        }
        return Ok(current);
    }
    ty.clone_with_control(control).map_err(Into::into)
}

/// Resolve an ALTER declaration after its relation and column checks. The compiler keeps modifier syntax intact until this boundary, including declarations an `IF NOT EXISTS` action can skip.
pub fn resolve_alter_column_type(
    resolver: &dyn FunctionTypeResolver,
    ty: &ColumnType,
) -> Result<ColumnType, SQLError> {
    match ty {
        ColumnType::Named(name) => {
            let ty = crate::compiler::compile_retained_type_declaration(name)?;
            resolve_declared_column_type(resolver, &ty)
        }
        ColumnType::Array(element) => resolve_alter_column_type(resolver, element)
            .map(|element| ColumnType::Array(Box::new(element))),
        other => resolve_declared_column_type(resolver, other),
    }
}
