//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain admitted type descriptions without retaining converted values or callbacks.

use super::{
    cast_value_with_resolution_with_control, cast_with_resolved_types, optional_type_name,
    resolve_catalog_type_with_control, ColumnType, EngineHook, Produced, ProductionControl,
    ResolvedCast, Result, Value,
};
use std::sync::Arc;

/// An immutable catalog's identity for already bound or schema-qualified type names. A hook must replace this identity when those names can resolve differently; unqualified search-path-dependent names are excluded by the SQL owner.
#[derive(Clone, Debug, Default)]
pub struct CatalogCastScope(Arc<()>);

impl CatalogCastScope {
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug)]
pub(super) struct CastTypes {
    pub(super) target: Option<Produced<ColumnType>>,
    pub(super) source: Option<Produced<ColumnType>>,
    pub(super) parsed_target: Option<Produced<ColumnType>>,
}

impl CastTypes {
    pub(super) fn resolve(
        source: Option<&str>,
        target: &str,
        engine: Option<&dyn EngineHook>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let target_type = resolve_catalog_type_with_control(target, engine, control)?;
        control.check()?;
        let source = source
            .map(|source| resolve_catalog_type_with_control(source, engine, control))
            .transpose()?
            .flatten();
        let parsed_target = if target_type.is_some() {
            None
        } else {
            optional_type_name(target, control)?
        };
        Ok(Self {
            target: target_type,
            source,
            parsed_target,
        })
    }
}

/// Successfully resolved metadata for one expression's fixed source/target spellings. Owned descriptors retain their original production allowances; values, input routines, domain checks and session settings remain per evaluation.
#[derive(Debug)]
pub struct RetainedCatalogCast {
    scope: CatalogCastScope,
    types: CastTypes,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl RetainedCatalogCast {
    /// Decline before conversion when the invoking immutable catalog changed. The caller must keep the original expression and its complete type-schema identity, and evaluate the operand before checking this scope.
    pub fn evaluate(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &str,
        engine: Option<&dyn EngineHook>,
        control: &ProductionControl<'_>,
    ) -> Option<Result<Produced<Value>>> {
        let current = engine?.catalog_cast_scope()?;
        self.scope.matches(current).then(|| {
            control.check()?;
            cast_with_resolved_types(
                value,
                source,
                target,
                engine,
                control,
                &self.types,
                &mut None,
            )
        })
    }
}

/// The original conversion result and optional immutable metadata, never a cached row value.
pub struct ScopedCastResult {
    pub value: Produced<Value>,
    pub resolved: Option<ResolvedCast>,
    pub catalog: Option<Box<RetainedCatalogCast>>,
}

/// Resolve both names at their original boundary and retain the successful descriptors only while an explicit immutable catalog capability remains valid. Hooks without that capability keep live name resolution.
pub fn cast_value_with_scoped_resolution(
    value: &Value,
    source: Option<&str>,
    target: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<ScopedCastResult> {
    control.check()?;
    // Explicit builtin scalar selections already have the smaller existing execution descriptor.
    if source
        .and_then(crate::type_resolution::pg_catalog_builtin_type)
        .zip(crate::type_resolution::pg_catalog_builtin_type(target))
        .is_some_and(|(source, target)| ResolvedCast::select(&source, &target).is_some())
    {
        let (value, resolved) =
            cast_value_with_resolution_with_control(value, source, target, engine, control)?;
        return Ok(ScopedCastResult {
            value,
            resolved,
            catalog: None,
        });
    }
    let scope = engine
        .and_then(EngineHook::catalog_cast_scope)
        .filter(|_| fixed_name(target) && source.is_none_or(fixed_name))
        .cloned();
    let types = CastTypes::resolve(source, target, engine, control)?;
    let mut resolved = None;
    let value = cast_with_resolved_types(
        value,
        source,
        target,
        engine,
        control,
        &types,
        &mut resolved,
    )?;
    // A domain/input callback can change the hook's catalog. Its already evaluated result remains valid, but the earlier names must not be published under the new identity.
    let scope = scope.filter(|scope| {
        engine
            .and_then(EngineHook::catalog_cast_scope)
            .is_some_and(|current| scope.matches(current))
    });
    let catalog = if let Some(scope) = scope {
        match control.reserve(std::mem::size_of::<RetainedCatalogCast>()) {
            Ok(memory) => Some(Box::new(RetainedCatalogCast {
                scope,
                types,
                _memory: memory,
            })),
            Err(uqa_core::ValueRetentionError::Memory(_)) => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    Ok(ScopedCastResult {
        value,
        resolved,
        catalog,
    })
}

fn fixed_name(name: &str) -> bool {
    crate::ast::UserTypeIdentity::parse(name).is_some()
        || crate::parse_regtype_name(name)
            .ok()
            .flatten()
            .is_some_and(|name| name.names.len() == 2)
}

#[cfg(test)]
mod tests;
