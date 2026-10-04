//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine type references, polymorphic signatures, and declaration validation.

use crate::{
    ast::{
        AlterRoutineStmt, ColumnDef, ColumnType, CreateFunction, FunctionBody, FunctionParamMode,
        FunctionReturns, RoutineColumnTypeReference,
    },
    type_resolution::canonical_routine_type_name,
    SQLError,
};

pub trait RoutineTypeCatalog {
    fn try_describe_table(&self, reference: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType>;
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError>;
    fn resolve_catalog_user_type_by_oid(&self, oid: u32) -> Option<ColumnType>;
    /// `USAGE` on a type a routine declares, as [`crate::FunctionTypeResolver::require_type_usage`] requires it.
    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError>;
    /// The name `PostgreSQL`'s `format_type_be` gives a type in messages.
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError>;
}

/// Resolve the declared argument and result types, as `interpret_function_parameter_list` and `compute_return_type` do. Each argument in order requires `USAGE` on its type, a missing one named as written and unquoted; then no input may follow a VARIADIC argument, nor an output in a procedure, a VARIADIC argument must be an array, a name may not repeat within one direction, only inputs may have defaults, and after one every input needs one, as every procedure output does not. The result type follows, a missing one quoted.
pub fn resolve_routine_type_references(
    catalog: &dyn RoutineTypeCatalog,
    def: &mut CreateFunction,
) -> Result<(), SQLError> {
    let mut have_defaults = false;
    let mut after_variadic = false;
    for index in 0..def.params.len() {
        let (previous, rest) = def.params.split_at_mut(index);
        let parameter = &mut rest[0];
        let written = parameter.written_type.take();
        parameter.type_name = resolve_used_routine_type(
            catalog,
            &parameter.type_name,
            ROUTINE_PARAMETER_PSEUDO_TYPES,
            parameter.type_reference.as_ref(),
            MissingRoutineType::Parameter(written.as_deref()),
        )?;
        parameter.type_reference = None;
        let input = !matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::Table
        );
        let output = !matches!(
            parameter.mode,
            FunctionParamMode::In | FunctionParamMode::Variadic
        );
        if input && after_variadic {
            return Err(routine_definition_error(
                "VARIADIC parameter must be the last input parameter",
            ));
        }
        if output && def.is_procedure && after_variadic {
            return Err(routine_definition_error(
                "VARIADIC parameter must be the last parameter",
            ));
        }
        if parameter.mode == FunctionParamMode::Variadic {
            after_variadic = true;
            if !variadic_type_is_array(catalog, &parameter.type_name) {
                return Err(routine_definition_error(
                    "VARIADIC parameter must be an array",
                ));
            }
        }
        if !parameter.name.is_empty()
            && previous.iter().any(|earlier| {
                earlier.name == parameter.name
                    && parameter_names_conflict(parameter.mode, earlier.mode)
            })
        {
            return Err(routine_definition_error(format!(
                "parameter name \"{}\" used more than once",
                parameter.name
            )));
        }
        if parameter.default.is_some() {
            if !input {
                return Err(routine_definition_error(
                    "only input parameters can have default values",
                ));
            }
            have_defaults = true;
        } else if input && have_defaults {
            return Err(routine_definition_error(
                "input parameters after one with a default value must also have defaults",
            ));
        } else if def.is_procedure && have_defaults {
            return Err(routine_definition_error(
                "procedure OUT parameters cannot appear after one with a default value",
            ));
        }
    }
    let written = def.return_written_type.take();
    match &mut def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            *type_name = resolve_used_routine_type(
                catalog,
                type_name,
                ROUTINE_RESULT_PSEUDO_TYPES,
                def.return_type_reference.as_ref(),
                MissingRoutineType::Result(written.as_deref()),
            )?;
        }
        FunctionReturns::None | FunctionReturns::Table => {}
    }
    def.return_type_reference = None;
    Ok(())
}

/// Whether two parameters of these modes may not share a name: a pure input never conflicts with a pure output.
fn parameter_names_conflict(current: FunctionParamMode, earlier: FunctionParamMode) -> bool {
    let pure_input = |mode| matches!(mode, FunctionParamMode::In | FunctionParamMode::Variadic);
    let pure_output = |mode| matches!(mode, FunctionParamMode::Out | FunctionParamMode::Table);
    !(pure_input(current) && pure_output(earlier) || pure_input(earlier) && pure_output(current))
}

/// Whether a VARIADIC argument's type is one `interpret_function_parameter_list` accepts: an array, `anyarray`, `anycompatiblearray` or `"any"`.
fn variadic_type_is_array(catalog: &dyn RoutineTypeCatalog, type_name: &str) -> bool {
    canonical_routine_type_name(type_name) == "any"
        || routine_declaration_is_array(catalog, type_name)
}

/// How a missing declared type is reported: `interpret_function_parameter_list` names an argument's type unquoted, `compute_return_type` quotes the result type, each as the statement wrote it.
#[derive(Clone, Copy)]
enum MissingRoutineType<'a> {
    Parameter(Option<&'a str>),
    Result(Option<&'a str>),
}

impl MissingRoutineType<'_> {
    /// The error for the missing type `type_name`, spelled as written when the statement's spelling is known.
    fn error(self, type_name: &str) -> SQLError {
        let (written, quoted) = match self {
            Self::Parameter(written) => (written, false),
            Self::Result(written) => (written, true),
        };
        let name = written.map_or_else(|| type_name.to_string(), str::to_string);
        SQLError::Routine {
            sqlstate: "42704".into(),
            message: if quoted {
                format!("type \"{name}\" does not exist")
            } else {
                format!("type {name} does not exist")
            },
        }
    }
}

pub fn resolve_alter_routine_identity_types(
    catalog: &dyn RoutineTypeCatalog,
    stmt: &AlterRoutineStmt,
) -> Result<Option<Vec<String>>, SQLError> {
    resolve_routine_identity_types(
        catalog,
        stmt.arg_types.as_deref(),
        &stmt.arg_type_references,
        "ALTER routine",
    )
}

pub fn resolve_routine_identity_types(
    catalog: &dyn RoutineTypeCatalog,
    types: Option<&[String]>,
    references: &[Option<RoutineColumnTypeReference>],
    context: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    let Some(types) = types else {
        if !references.is_empty() {
            return Err(SQLError::Internal(format!(
                "{context} omitted its identity types but retained type references"
            )));
        }
        return Ok(None);
    };
    if !references.is_empty() && references.len() != types.len() {
        return Err(SQLError::Internal(format!(
            "{context} has {} identity types but {} type references",
            types.len(),
            references.len()
        )));
    }
    types
        .iter()
        .enumerate()
        .map(|(index, type_name)| {
            resolve_routine_type_name_with_reference(
                catalog,
                type_name,
                ROUTINE_PARAMETER_PSEUDO_TYPES,
                references.get(index).and_then(Option::as_ref),
            )
            .map(|resolved| canonical_routine_type_name(&resolved))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

const POLYMORPHIC_PSEUDO_TYPES: &[&str] = &[
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

const ROUTINE_PARAMETER_PSEUDO_TYPES: &[&str] = &[
    "record",
    "refcursor",
    "cstring",
    "any",
    "void",
    "trigger",
    "internal",
    "event_trigger",
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

const ROUTINE_RESULT_PSEUDO_TYPES: &[&str] = &[
    "record",
    "refcursor",
    "cstring",
    "any",
    "void",
    "trigger",
    "internal",
    "event_trigger",
    "anyelement",
    "anyarray",
    "anynonarray",
    "anyenum",
    "anyrange",
    "anymultirange",
    "anycompatible",
    "anycompatiblearray",
    "anycompatiblenonarray",
    "anycompatiblerange",
    "anycompatiblemultirange",
];

/// A declared routine type: a pseudo-type by name, or a catalog type.
enum DeclaredRoutineType {
    Pseudo(String),
    Catalog(ColumnType),
}

impl DeclaredRoutineType {
    /// A user-defined type is recorded by identity, so the signature survives renames and does not depend on the search path.
    fn into_catalog_name(self) -> String {
        match self {
            Self::Pseudo(name) => name,
            Self::Catalog(ty) => ty.catalog_name(),
        }
    }
}

/// Resolve a declared argument or result type and require `USAGE` on it. Pseudo-types keep their default privileges.
fn resolve_used_routine_type(
    catalog: &dyn RoutineTypeCatalog,
    type_name: &str,
    allowed_pseudo_types: &[&str],
    structured_reference: Option<&RoutineColumnTypeReference>,
    missing: MissingRoutineType<'_>,
) -> Result<String, SQLError> {
    let declared = resolve_declared_routine_type(
        catalog,
        type_name,
        allowed_pseudo_types,
        structured_reference,
        Some(missing),
    )?;
    if let DeclaredRoutineType::Catalog(ty) = &declared {
        catalog.require_type_usage(ty)?;
    }
    Ok(declared.into_catalog_name())
}

fn resolve_routine_type_name_with_reference(
    catalog: &dyn RoutineTypeCatalog,
    type_name: &str,
    allowed_pseudo_types: &[&str],
    structured_reference: Option<&RoutineColumnTypeReference>,
) -> Result<String, SQLError> {
    resolve_declared_routine_type(
        catalog,
        type_name,
        allowed_pseudo_types,
        structured_reference,
        None,
    )
    .map(DeclaredRoutineType::into_catalog_name)
}

fn resolve_declared_routine_type(
    catalog: &dyn RoutineTypeCatalog,
    type_name: &str,
    allowed_pseudo_types: &[&str],
    structured_reference: Option<&RoutineColumnTypeReference>,
    missing: Option<MissingRoutineType<'_>>,
) -> Result<DeclaredRoutineType, SQLError> {
    let mut base = type_name.trim();
    let mut array_dimensions = 0usize;
    while let Some(element) = base.strip_suffix("[]") {
        base = element.trim_end();
        array_dimensions += 1;
    }
    let resolved = if base
        .get(base.len().saturating_sub("%type".len())..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("%type"))
    {
        let reference = structured_reference.ok_or_else(|| {
            SQLError::Internal(format!(
                "routine type reference `{type_name}` is missing structured relation-column identity"
            ))
        })?;
        let table = reference.relation_reference();
        let columns = catalog
            .try_describe_table(&table)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "resolve routine type reference `{type_name}`: {error}"
                ))
            })?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        columns
            .into_iter()
            .find(|definition| definition.name == reference.column)
            .map(|definition| definition.ty)
            .ok_or_else(|| SQLError::UnknownColumn(reference.type_reference()))?
    } else {
        let canonical = canonical_routine_type_name(base);
        if allowed_pseudo_types.contains(&canonical.as_str()) {
            if array_dimensions != 0 {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("type `{type_name}` does not exist"),
                });
            }
            return Ok(DeclaredRoutineType::Pseudo(canonical));
        }
        match missing {
            Some(missing) if catalog.resolve_catalog_column_type(base).is_none() => {
                return Err(missing.error(type_name));
            }
            _ => catalog.resolve_catalog_column_type_name(base)?,
        }
    };
    let mut resolved = resolved;
    for _ in 0..array_dimensions {
        resolved = ColumnType::Array(Box::new(resolved));
    }
    Ok(DeclaredRoutineType::Catalog(resolved))
}

pub fn resolve_plpgsql_datum_types(
    catalog: &dyn RoutineTypeCatalog,
    function: &mut crate::plpgsql::PLpgSQLFunction,
) -> Result<(), SQLError> {
    for datum in &mut function.datums {
        let crate::plpgsql::PLpgSQLDatum::Var(variable) = datum else {
            continue;
        };
        if variable.type_reference.is_none() {
            if let Some(ty) = variable
                .type_oid
                .and_then(|oid| catalog.resolve_catalog_user_type_by_oid(oid))
            {
                // The compiled function names the variable's type by identity, as a compiled PL/pgSQL function holds type OIDs: a later rename does not change which type it means.
                variable.type_name = ty.catalog_name();
                continue;
            }
        }
        variable.type_name = resolve_routine_type_name_with_reference(
            catalog,
            &variable.type_name,
            &[
                "record",
                "refcursor",
                "anyelement",
                "anyarray",
                "anynonarray",
                "anyenum",
                "anyrange",
                "anymultirange",
                "anycompatible",
                "anycompatiblearray",
                "anycompatiblenonarray",
                "anycompatiblerange",
                "anycompatiblemultirange",
            ],
            variable.type_reference.as_ref(),
        )?;
        variable.type_reference = None;
    }
    Ok(())
}

pub(super) fn validate_routine_declaration(def: &CreateFunction) -> Result<(), SQLError> {
    let inputs = validate_routine_input_types(def)?;
    if matches!(def.body, FunctionBody::Statements(_)) && inputs.any {
        return Err(routine_definition_error(
            "SQL function with unquoted function body cannot have polymorphic arguments",
        ));
    }
    validate_routine_output_types(def, &inputs)
}

pub(super) fn routine_parameter_regrole_constants(
    catalog: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
) -> crate::catalog::regrole_dependencies::StoredRegroleConstants {
    let mut constants = crate::catalog::regrole_dependencies::StoredRegroleConstants::default();
    for parameter in &def.params {
        let Some(default) = parameter.default.as_ref() else {
            continue;
        };
        let target = catalog
            .resolve_catalog_column_type(&parameter.type_name)
            .or_else(|| ColumnType::from_sql_name(&parameter.type_name).ok());
        constants.collect_expression(default, target.as_ref());
    }
    constants
}

#[derive(Default)]
struct PolymorphicInputs {
    simple: bool,
    compatible: bool,
    any: bool,
}

fn validate_routine_input_types(def: &CreateFunction) -> Result<PolymorphicInputs, SQLError> {
    let mut inputs = PolymorphicInputs::default();
    for parameter in &def.params {
        let type_name = canonical_routine_type_name(&parameter.type_name);
        let is_input = matches!(
            parameter.mode,
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
        );
        if let Some(family) = polymorphic_family(&type_name) {
            inputs.any |= is_input;
            if is_input {
                match family {
                    RoutinePolymorphicFamily::Simple => inputs.simple = true,
                    RoutinePolymorphicFamily::Compatible => inputs.compatible = true,
                }
            }
            continue;
        }
        if ROUTINE_PARAMETER_PSEUDO_TYPES.contains(&type_name.as_str()) {
            let supported = match type_name.as_str() {
                "record" => !is_input || def.language == "plpgsql",
                "refcursor" => true,
                _ => false,
            };
            if !supported {
                return Err(pseudo_type_error(
                    def,
                    format!("cannot have arguments of type {type_name}"),
                    format!("cannot accept type {type_name}"),
                ));
            }
        }
    }
    Ok(inputs)
}

fn validate_routine_output_types(
    def: &CreateFunction,
    inputs: &PolymorphicInputs,
) -> Result<(), SQLError> {
    let mut output_types = def
        .output_params()
        .into_iter()
        .map(|parameter| parameter.type_name.as_str())
        .collect::<Vec<_>>();
    if let FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } =
        &def.returns
    {
        output_types.push(type_name);
    }
    for output_type in output_types {
        let type_name = canonical_routine_type_name(output_type);
        match polymorphic_family(&type_name) {
            Some(RoutinePolymorphicFamily::Simple) if !inputs.simple => {
                return Err(routine_definition_error(format!(
                    "cannot determine result data type: a result of type {type_name} requires at least one simple polymorphic input"
                )));
            }
            Some(RoutinePolymorphicFamily::Compatible) if !inputs.compatible => {
                return Err(routine_definition_error(format!(
                    "cannot determine result data type: a result of type {type_name} requires at least one compatible polymorphic input"
                )));
            }
            None if ROUTINE_RESULT_PSEUDO_TYPES.contains(&type_name.as_str())
                && !matches!(type_name.as_str(), "record" | "refcursor" | "void")
                && !(type_name == "trigger"
                    && def.language == "plpgsql"
                    && !def.is_procedure
                    && matches!(def.returns, FunctionReturns::Scalar { .. })) =>
            {
                let message = format!("cannot return type {type_name}");
                return Err(pseudo_type_error(def, message.clone(), message));
            }
            Some(_) | None => {}
        }
    }
    Ok(())
}

/// A pseudo-type the routine's language rejects, reported as its validator reports it: `fmgr_sql_validator` as an invalid definition, and `plpgsql_validator` as an unsupported feature.
fn pseudo_type_error(def: &CreateFunction, sql: String, plpgsql: String) -> SQLError {
    if def.language == "plpgsql" {
        SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("PL/pgSQL functions {plpgsql}"),
        }
    } else {
        routine_definition_error(format!("SQL functions {sql}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutinePolymorphicFamily {
    Simple,
    Compatible,
}

fn polymorphic_family(type_name: &str) -> Option<RoutinePolymorphicFamily> {
    if !POLYMORPHIC_PSEUDO_TYPES.contains(&type_name) {
        return None;
    }
    Some(if type_name.starts_with("anycompatible") {
        RoutinePolymorphicFamily::Compatible
    } else {
        RoutinePolymorphicFamily::Simple
    })
}

fn routine_declaration_is_array(catalog: &dyn RoutineTypeCatalog, type_name: &str) -> bool {
    let canonical = canonical_routine_type_name(type_name);
    canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
        || catalog
            .resolve_catalog_column_type(&canonical)
            .is_some_and(|ty| routine_column_type_is_array(&ty))
}

fn routine_column_type_is_array(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(_) | ColumnType::AnyArray => true,
        ColumnType::Domain { base, .. } => routine_column_type_is_array(base),
        _ => false,
    }
}

pub(super) fn routine_definition_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: message.into(),
    }
}
