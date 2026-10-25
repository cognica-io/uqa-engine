//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware cast selection shares controlled type, array and value constructors.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    ArrayValue, Value,
};

use super::super::casting::{cast_value_from_with_control, parse_pg_array_literal_with_input};
use super::super::ResolvedCast;
use super::{format_regtype_elements_with_control, format_regtype_value_with_control, EngineHook};
use crate::{
    ast::ColumnType,
    error::{Result, SQLError},
};

mod datestyle;
mod intervalstyle;
mod metadata;
pub use metadata::{
    cast_value_with_scoped_resolution, CatalogCastScope, RetainedCatalogCast, ScopedCastResult,
};
mod ranges;
mod timezone;
mod xml;

/// Catalog input needed after analysis selects a type for an unknown literal. Domain array input includes each element's constraints, so the caller must retain this value rather than read the text again at execution.
pub trait CatalogInputFunctions {
    fn read_unknown_input(&self, text: &str, target: &ColumnType) -> Result<Value>;
}

pub(crate) fn requires_domain_array_input(target: &ColumnType) -> bool {
    matches!(target, ColumnType::Array(element) if matches!(array_leaf_type(element), ColumnType::Domain { .. }))
}

/// Input constants containing declared composites/ranges, domain arrays or XML need the definition's catalog or session input context.
pub(crate) fn requires_catalog_constant_input(target: &ColumnType) -> bool {
    requires_domain_array_input(target)
        || match target {
            ColumnType::Xml
            | ColumnType::Composite(_)
            | ColumnType::DeclaredRange(_)
            | ColumnType::DeclaredMultirange(_) => true,
            ColumnType::Array(element) | ColumnType::Domain { base: element, .. } => {
                requires_catalog_constant_input(element)
            }
            _ => false,
        }
}

/// Read a catalog-dependent constant once, retaining its typed datum in stored syntax.
pub fn read_catalog_input(
    text: &str,
    target: &ColumnType,
    engine: &dyn EngineHook,
) -> Result<Value> {
    if requires_domain_array_input(target) {
        return read_catalog_array_input(text, target, engine);
    }
    if !requires_catalog_constant_input(target) {
        return Err(SQLError::Internal(
            "constant input requires a catalog or session context".into(),
        ));
    }
    super::cast_value_with_type_resolution(
        &Value::Str(text.into()),
        Some("unknown"),
        &target.catalog_name(),
        Some(engine),
    )
}

/// Read an array whose elements are domains with the same input and constraint path as a catalog-aware cast. The resolved target preserves domain identity; a scalar outer domain remains the surrounding expression's runtime coercion.
pub fn read_catalog_array_input(
    text: &str,
    target: &ColumnType,
    engine: &dyn EngineHook,
) -> Result<Value> {
    if !requires_domain_array_input(target) {
        return Err(SQLError::Internal(
            "catalog array input requires domain elements".into(),
        ));
    }
    let control = ProductionControl::uncontrolled();
    let element = array_leaf_type(target);
    let target_name = element.catalog_name_with_control(&control)?;
    let array = parse_pg_array_literal_with_input(text, &control, |value| {
        cast_value_with_type_resolution_with_control(
            &value,
            None,
            &target_name,
            Some(engine),
            &control,
        )
    })?;
    let (array, memory) = array.into_parts();
    let oid = crate::catalog::type_metadata::pg_type_oid(element) as u32;
    control
        .finish(Value::Array(array.with_element_type_oid(Some(oid))), memory)?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary catalog array input owner".into()))
}

#[must_use]
pub fn coercion_type_name(ty: &ColumnType) -> String {
    coercion_type_name_with_control(ty, &ProductionControl::uncontrolled())
        .expect("ordinary coercion type name")
        .into_uncontrolled()
        .expect("ordinary coercion type name owner")
}

fn coercion_type_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    match ty {
        ColumnType::Domain { base, .. } => coercion_type_name_with_control(base, control),
        ColumnType::Array(element) => {
            let element = coercion_type_name_with_control(element, control)?;
            let mut name = ProductionString::new(*control);
            name.push_str(&element)?;
            name.push_str("[]")?;
            Ok(name.finish()?)
        }
        _ => Ok(ty.sql_name_with_control(control)?),
    }
}

/// The types of `PostgreSQL`'s string category, which any type casts to through its output function.
fn is_string_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Name
            | ColumnType::Varchar(_)
            | ColumnType::Character(_)
            | ColumnType::Bpchar
    )
}

fn regrole_array_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => {
            matches!(element.as_ref(), ColumnType::Regrole) || regrole_array_type(element)
        }
        _ => false,
    }
}

fn array_leaf_type(ty: &ColumnType) -> &ColumnType {
    match ty {
        ColumnType::Array(element) => array_leaf_type(element),
        _ => ty,
    }
}

fn optional_type_name(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>> {
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Cast a value after resolving catalog-owned types and enforcing domain constraints before exposing a base-type carrier.
pub fn cast_value_with_type_resolution(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
) -> Result<Value> {
    cast_value_with_type_resolution_with_control(
        value,
        source_ty,
        target_ty,
        engine,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary catalog cast owner".into()))
}

/// Container-to-string coercion observes each field or element through its SQL output function. It does not repeat input or domain checks.
fn container_output_cast(
    value: &Value,
    source: Option<&ColumnType>,
    target_ty: &str,
    target: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if !matches!(value, Value::Array(_) | Value::Row(_) | Value::Record(_))
        || !is_string_type(target)
    {
        return Ok(None);
    }
    let unspecified;
    let source = if let Some(source) = source {
        source
    } else {
        unspecified = if matches!(value, Value::Array(_)) {
            ColumnType::array_with_control(
                control.finish(ColumnType::Text, control.empty_reservation())?,
                control,
            )?
        } else {
            control.finish(ColumnType::Record, control.empty_reservation())?
        };
        &unspecified
    };
    let text =
        crate::result::format_postgres_text_with_control(value, source, Some(engine), control)?;
    let (text, memory) = text.into_parts();
    let text = control.finish(Value::Str(text), memory)?;
    if matches!(target, ColumnType::Text) {
        return Ok(Some(text));
    }
    cast_value_from_with_control(&text, target_ty, Some("text"), control).map(Some)
}

fn physical_output_cast(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    target: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    let Value::Datum(datum) = value else {
        return Ok(None);
    };
    if !is_string_type(target) {
        return Ok(None);
    }
    let decoded =
        super::super::datums::read_with_catalog_and_control(datum, Some(engine), control)?;
    cast_value_with_type_resolution_with_control(
        &decoded,
        source_ty,
        target_ty,
        Some(engine),
        control,
    )
    .map(Some)
}

/// Resolve catalog inputs at their external handoff, then admit SQL-owned names, element conversions and output before constructing them.
pub fn cast_value_with_type_resolution_with_control(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    cast_with_resolution(value, source_ty, target_ty, engine, control, &mut None)
}

/// Return an immutable numeric conversion when the ordinary catalog-aware cast selected one. Callers may retain it only within the same prepared expression and type-schema scope; the first evaluation keeps the original resolution, callback and failure order.
pub fn cast_value_with_numeric_resolution_with_control(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Value>, Option<super::super::NumericCast>)> {
    let (output, resolved) =
        cast_value_with_resolution_with_control(value, source_ty, target_ty, engine, control)?;
    Ok((output, resolved.and_then(|cast| cast.numeric())))
}

/// Return the builtin scalar selection reached by an ordinary successful conversion. The caller may reuse it only for the same immutable expression and type-schema identity with explicit builtin type names.
pub fn cast_value_with_resolution_with_control(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Value>, Option<ResolvedCast>)> {
    let mut resolved = None;
    let output = cast_with_resolution(value, source_ty, target_ty, engine, control, &mut resolved)?;
    Ok((output, resolved))
}

fn cast_with_resolution(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    resolved: &mut Option<ResolvedCast>,
) -> Result<Produced<Value>> {
    let types = metadata::CastTypes::resolve(source_ty, target_ty, engine, control)?;
    cast_with_resolved_types(
        value, source_ty, target_ty, engine, control, &types, resolved,
    )
}

fn cast_with_resolved_types(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    types: &metadata::CastTypes,
    resolved: &mut Option<ResolvedCast>,
) -> Result<Produced<Value>> {
    let resolved_target = &types.target;
    let resolved_source_type = &types.source;
    let parsed_target = &types.parsed_target;
    let source_type = resolved_source_type.as_deref();
    let target_column_type = resolved_target.as_deref().or(parsed_target.as_deref());
    let _date_order = datestyle::input_scope(value, target_column_type, engine, control)?;
    let _interval_style = intervalstyle::cast_scope(value, target_column_type, engine, control)?;
    if let (Some(engine), Some(target)) = (engine, resolved_target.as_deref()) {
        if let Some(value) = record_identity_cast(value, source_type, target, engine, control)? {
            return Ok(value);
        }
        if let Some(output) =
            physical_output_cast(value, source_ty, target_ty, target, engine, control)?
        {
            return Ok(output);
        }
        if let Some(value) =
            cast_defined_type(engine, value, source_ty, source_type, target, control)?
        {
            return Ok(value);
        }
        if let Some(output) =
            container_output_cast(value, source_type, target_ty, target, engine, control)?
        {
            return Ok(output);
        }
        if let Value::Enum(label) = value {
            let output =
                super::super::enums::enum_output_for_cast(engine.enum_labels(), label, target)?;
            return cast_value_with_type_resolution_with_control(
                &output,
                Some("text"),
                target_ty,
                Some(engine),
                control,
            );
        }
        control.check()?;
        if matches!(target, ColumnType::Array(_))
            && (requires_catalog_array_cast(target)
                || source_type.is_some_and(|source| {
                    crate::type_resolution::cast_volatility(source, target)
                        != crate::ast::FunctionVolatility::Immutable
                })
                || super::super::enums::contains_enum_carrier(value)
                || resolved_source_type
                    .as_deref()
                    .is_some_and(super::super::enums::is_enum_bearing))
        {
            return cast_catalog_array(value, source_ty, target, engine, control);
        }
    }
    if let (Some(source), Some(target)) = (source_type, target_column_type) {
        if let Some(cast) = ResolvedCast::select(source, target).filter(|cast| cast.accepts(value))
        {
            let output = evaluate_selected_value(&cast, value, source_ty, engine, control)?;
            *resolved = Some(cast);
            return Ok(output);
        }
    }
    let resolved_source = resolved_source_type
        .as_ref()
        .map(|ty| coercion_type_name_with_control(ty, control))
        .transpose()?;
    let source_ty = resolved_source.as_deref().map(String::as_str).or(source_ty);
    let target_name = resolved_target
        .as_ref()
        .map(|ty| coercion_type_name_with_control(ty, control))
        .transpose()?;
    let target_ty = target_name.as_ref().map_or(target_ty, |name| name.as_str());
    cast_resolved_value(
        value,
        source_ty,
        target_ty,
        target_column_type,
        engine,
        control,
    )
}

/// Reuse a selected builtin conversion while preserving live setting scopes before callbacks and conversion.
pub(crate) fn evaluate_selected_cast(
    cast: &ResolvedCast,
    value: &Value,
    source_name: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let target = cast.target_type();
    let _date_order = datestyle::input_scope(value, Some(&target), engine, control)?;
    let _interval_style = intervalstyle::cast_scope(value, Some(&target), engine, control)?;
    if let Some(engine) = engine {
        if let Some(output) = engine.cast_domain(value, Some(source_name), &target)? {
            return Ok(control.retain_external_value(output)?);
        }
    }
    evaluate_selected_value(cast, value, Some(source_name), engine, control)
}

fn evaluate_selected_value(
    cast: &ResolvedCast,
    value: &Value,
    source_name: Option<&str>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let target = cast.target_type();
    if let Some(output) = datestyle::date_text(value, Some(&target), engine, control)? {
        let (text, memory) = output.into_parts();
        if matches!(target, ColumnType::Text) {
            return Ok(control.finish(Value::Str(text), memory)?);
        }
        return cast.evaluate_from(&Value::Str(text), Some("text"), control);
    }
    let zoned = timezone::cast_timestamp(value, Some(&target), engine, control)?;
    cast.evaluate_from(zoned.as_ref().unwrap_or(value), source_name, control)
}

/// Declared type input and identity coercion precede generic output and array casts. Domain checks keep their established first position.
fn cast_defined_type(
    engine: &dyn EngineHook,
    value: &Value,
    source_name: Option<&str>,
    source: Option<&ColumnType>,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if let Some(value) = engine.cast_domain(value, source_name, target)? {
        return Ok(Some(control.retain_external_value(value)?));
    }
    if matches!(target, ColumnType::Xml) {
        return xml::cast(value, source_name, source, engine, control).map(Some);
    }
    if let Some(value) = ranges::cast(engine, value, source, target, control)? {
        return Ok(Some(value));
    }
    if let Some(value) = super::super::enums::cast_to_enum_with_control(
        engine.enum_labels(),
        value,
        source,
        target,
        control,
    )? {
        return Ok(Some(value));
    }
    super::super::composites::cast_to_composite(engine, value, source, target)?
        .map(|value| control.retain_external_value(value).map_err(SQLError::from))
        .transpose()
}

fn resolve_catalog_type_with_control(
    name: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>> {
    engine
        .map(|engine| engine.resolve_type_name(name))
        .transpose()
        .map_err(SQLError::Internal)?
        .flatten()
        .map(|ty| {
            ty.retain_external_with_control(control)
                .map_err(SQLError::from)
        })
        .transpose()
}

fn record_identity_cast(
    value: &Value,
    resolved_source_type: Option<&ColumnType>,
    target: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    if matches!(target, ColumnType::Record) {
        // Composite-to-record coercion is a binary relabel: even malformed physical fields remain unread.
        if let Value::Datum(datum) = value {
            if crate::expr::composites::datum_is_composite(datum, engine)? {
                return Ok(Some(control.copy_value(value)?));
            }
        }
        if let (Value::Record(record), Some(ColumnType::Composite(source))) =
            (value, resolved_source_type)
        {
            let (value, memory) = control.copy_value(value)?.into_parts();
            let Value::Record(value) = value else {
                unreachable!();
            };
            return Ok(Some(control.finish(
                Value::Record(value.with_type_oid(Some(record.type_oid().unwrap_or(source.oid)))),
                memory,
            )?));
        }
    }

    Ok(None)
}

fn cast_resolved_value(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    target_column_type: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if target_column_type.is_some_and(regrole_array_type) {
        let source_column_type = source_ty
            .map(|name| optional_type_name(name, control))
            .transpose()?
            .flatten();
        let source_name = source_column_type
            .as_deref()
            .map(array_leaf_type)
            .map(|ty| ty.sql_name_with_control(control))
            .transpose()?;
        return cast_array(
            value,
            source_name.as_ref().map(|name| name.as_str()),
            &ColumnType::Regrole,
            "regrole[]",
            engine,
            control,
        );
    }
    // `find_coercion_pathway` casts an OID alias to a string type through its output function, as `CoerceViaIO`, and the string type then applies its own modifier; an array does so element by element.
    if let Some(ColumnType::Array(element)) = target_column_type {
        if is_string_type(element) {
            if let Some(source_ty) = source_ty
                .map(|source| optional_type_name(source, control))
                .transpose()?
                .flatten()
            {
                if let Some(elements) =
                    format_regtype_elements_with_control(value, &source_ty, engine, control)?
                {
                    let (elements, _memory) = elements.into_parts();
                    return cast_value_from_with_control(
                        &Value::Array(elements),
                        target_ty,
                        Some("text[]"),
                        control,
                    );
                }
            }
        }
    }
    let text_target = target_ty.eq_ignore_ascii_case("text");
    if text_target || target_column_type.is_some_and(is_string_type) {
        if let Some(source_ty) = source_ty
            .map(|source| optional_type_name(source, control))
            .transpose()?
            .flatten()
        {
            if let Some(text) =
                format_regtype_value_with_control(value, &source_ty, engine, control)?
            {
                let (text, memory) = text.into_parts();
                if text_target {
                    return Ok(control.finish(Value::Str(text), memory)?);
                }
                return cast_value_from_with_control(
                    &Value::Str(text),
                    target_ty,
                    Some("text"),
                    control,
                );
            }
        }
    }
    if let (Some(engine), Value::Str(name) | Value::FixedChar(name)) = (engine, value) {
        let oid = resolve_regobject_input(name, target_ty, target_column_type, engine, control)?;
        if let Some(oid) = oid {
            return Ok(control.finish(Value::Int(oid), control.empty_reservation())?);
        }
    }
    if let Some(output) =
        datestyle::date_output(value, target_ty, target_column_type, engine, control)?
    {
        return Ok(output);
    }
    let zoned = timezone::cast_timestamp(value, target_column_type, engine, control)?;
    cast_value_from_with_control(
        zoned.as_ref().unwrap_or(value),
        target_ty,
        source_ty,
        control,
    )
}

fn resolve_regobject_input(
    name: &str,
    target_ty: &str,
    target_column_type: Option<&ColumnType>,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>> {
    enum ObjectKind {
        Relation,
        RoutineName,
        Routine,
        Role,
        Namespace,
        Collation,
        Type,
    }
    let kind = if target_ty.eq_ignore_ascii_case("regclass") {
        ObjectKind::Relation
    } else if matches!(target_column_type, Some(ColumnType::Regproc))
        || target_ty.eq_ignore_ascii_case("regproc")
    {
        ObjectKind::RoutineName
    } else if target_ty.eq_ignore_ascii_case("regprocedure") {
        ObjectKind::Routine
    } else if matches!(target_column_type, Some(ColumnType::Regrole))
        || target_ty.eq_ignore_ascii_case("regrole")
    {
        ObjectKind::Role
    } else if matches!(target_column_type, Some(ColumnType::Regnamespace)) {
        ObjectKind::Namespace
    } else if matches!(target_column_type, Some(ColumnType::Regcollation)) {
        ObjectKind::Collation
    } else if matches!(target_column_type, Some(ColumnType::Regtype)) {
        ObjectKind::Type
    } else {
        return Ok(None);
    };
    let oid = match kind {
        ObjectKind::Relation => engine.resolve_regclass_input(name)?,
        // `regprocin` reports its own missing and ambiguous names.
        ObjectKind::RoutineName => return engine.resolve_regproc(name),
        ObjectKind::Routine => engine.resolve_regprocedure_input(name)?,
        ObjectKind::Role => engine.resolve_regrole(name)?,
        ObjectKind::Namespace => engine.resolve_regnamespace(name)?,
        ObjectKind::Collation => engine.resolve_regcollation(name)?,
        ObjectKind::Type => engine.resolve_regtype_input(name)?,
    };
    control.check()?;
    if oid.is_some() || matches!(kind, ObjectKind::Type) {
        return Ok(oid);
    }
    let (sqlstate, message) = match kind {
        ObjectKind::Relation => ("42P01", format!("relation \"{name}\" does not exist")),
        ObjectKind::Routine => ("42883", format!("function \"{name}\" does not exist")),
        ObjectKind::Role => ("42704", format!("role \"{name}\" does not exist")),
        ObjectKind::Namespace => ("3F000", format!("schema \"{name}\" does not exist")),
        ObjectKind::Collation => (
            "42704",
            format!("collation \"{name}\" for encoding \"UTF8\" does not exist"),
        ),
        ObjectKind::Type | ObjectKind::RoutineName => {
            unreachable!("regtype and regproc input return before reporting a missing object")
        }
    };
    Err(SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    })
}

/// Whether an array cast needs the catalog for its elements: domains, enums, composites and the OID alias types whose input functions read catalog names. `regrole[]` has its own path above.
fn requires_catalog_array_cast(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { .. }
        | ColumnType::Xml
        | ColumnType::Regtype
        | ColumnType::Regclass
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regcollation
        | ColumnType::Regnamespace
        | ColumnType::Enum(_)
        | ColumnType::Composite(_)
        | ColumnType::DeclaredRange(_)
        | ColumnType::DeclaredMultirange(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_) => true,
        ColumnType::Array(element) => requires_catalog_array_cast(element),
        _ => false,
    }
}

fn cast_catalog_array(
    value: &Value,
    source: Option<&str>,
    target: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if matches!(value, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let source_element = source.map(|name| name.trim_end_matches("[]"));
    let leaf = array_leaf_type(target);
    let target_name = target.sql_name_with_control(control)?;
    let output = cast_array(
        value,
        source_element,
        leaf,
        &target_name,
        Some(engine),
        control,
    )?;
    let (Value::Array(array), memory) = output.into_parts() else {
        return Err(SQLError::Internal(
            "catalog array cast returned a non-array".into(),
        ));
    };
    let oid = crate::catalog::type_metadata::pg_type_oid(leaf) as u32;
    Ok(control.finish(Value::Array(array.with_element_type_oid(Some(oid))), memory)?)
}

fn cast_array(
    value: &Value,
    source: Option<&str>,
    target_element: &ColumnType,
    target_name: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let vector_element = match value {
        Value::LegacyVector(vector) => Some(match vector.kind() {
            uqa_core::LegacyVectorKind::SmallInteger => ColumnType::SmallInteger,
            uqa_core::LegacyVectorKind::Oid => ColumnType::Oid,
        }),
        _ => None,
    };
    let vector_name = vector_element
        .as_ref()
        .map(|element| element.sql_name_with_control(control))
        .transpose()?;
    let source = vector_name.as_deref().map(String::as_str).or(source);
    let target_element_name = target_element.catalog_name_with_control(control)?;
    let array = if let Value::Str(text) = value {
        parse_pg_array_literal_with_input(text, control, |value| {
            cast_value_with_type_resolution_with_control(
                &value,
                source,
                &target_element_name,
                engine,
                control,
            )
        })?
    } else {
        let array = match value {
            Value::Array(array) => array,
            Value::LegacyVector(vector) => vector.as_array(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "CAST AS {target_name}: expected array, got {other:?}"
                )));
            }
        };
        let elements = cast_array_elements(
            array.elements(),
            source,
            &target_element_name,
            engine,
            control,
        )?;
        let normalize_empty =
            if let Some(source) = vector_element.filter(|_| array.elements().is_empty()) {
                source != *target_element
                    && !crate::type_resolution::cast_catalog_entry_with_control(
                        &source,
                        target_element,
                        control,
                    )?
                    .is_some_and(|entry| entry.method == crate::type_resolution::CastMethod::Binary)
            } else {
                false
            };
        if normalize_empty {
            ArrayValue::try_new_with_control(elements, control)?
        } else {
            rebuild_array(array, elements, control)?
        }
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions changed during cast".into()))?
    };
    let (array, memory) = array.into_parts();
    let oid = if matches!(target_element, ColumnType::Regrole) {
        Some(crate::catalog::type_metadata::pg_type_oid(&ColumnType::Regrole) as u32)
    } else {
        array.element_type_oid()
    };
    Ok(control.finish(Value::Array(array.with_element_type_oid(oid)), memory)?)
}

fn cast_array_elements(
    values: &[Value],
    source: Option<&str>,
    target: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = match value {
            Value::List(values) => {
                let (values, memory) =
                    cast_array_elements(values, source, target, engine, control)?.into_parts();
                control.finish(Value::List(values), memory)?
            }
            value => cast_value_with_type_resolution_with_control(
                value, source, target, engine, control,
            )?,
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

pub(super) fn rebuild_array(
    source: &ArrayValue,
    elements: Produced<Vec<Value>>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ArrayValue>>> {
    let mut bounds = ProductionVec::new(*control);
    bounds.reserve(source.lower_bounds().len())?;
    for bound in source.lower_bounds() {
        bounds.push_copy(*bound)?;
    }
    ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
        .map(|array| {
            let (array, memory) = array.into_parts();
            control
                .finish(
                    array.with_element_type_oid(source.element_type_oid()),
                    memory,
                )
                .map_err(Into::into)
        })
        .transpose()
}

#[cfg(test)]
mod tests;
