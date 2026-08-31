//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static user-routine signature matching and overload resolution.

use uqa_execution::{
    match_routine_signature, rank_function_matches, BuiltinFunctionOverload, FunctionTypeResolver,
    MatchedRoutineSignature, RankedFunctionMatch, ResolvedFunctionOverload, RoutineCallDescriptor,
    RoutineParameterDescriptor, RoutineSignatureMatchError,
};
use uqa_sql::ast::{
    ColumnType, CreateFunction, FunctionBinding, FunctionParamMode, FunctionReturns,
    RoutineInvocationBinding, RoutineVariadicMode,
};
use uqa_sql::SQLError;

use crate::{Arc, Engine, RelationIdentity};

use super::{canonical_routine_type_name, combined_overloads, SQLUserFunction};

/// Function-catalog operations required by static query binding. The interface deliberately excludes storage, transaction, locking, and execution services so the binder can run against a deterministic catalog fixture.
pub(crate) trait RoutineResolution: FunctionTypeResolver {
    fn has_registered_scalar_function(&self, _name: &str) -> bool {
        false
    }

    fn has_registered_table_function(&self, _name: &str) -> bool {
        false
    }

    fn has_registered_aggregate_function(&self, _name: &str) -> bool {
        false
    }

    fn lookup_sql_functions(&self, _name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        None
    }

    fn resolve_static_sql_function(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        Ok(None)
    }

    fn resolve_static_sql_function_match(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        Ok(None)
    }

    fn resolve_table_function_overload_with_builtins(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
        _builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for Engine {
    fn has_registered_scalar_function(&self, name: &str) -> bool {
        Engine::has_registered_scalar_function(self, name)
    }

    fn has_registered_table_function(&self, name: &str) -> bool {
        Engine::has_registered_table_function(self, name)
    }

    fn has_registered_aggregate_function(&self, name: &str) -> bool {
        Engine::has_registered_aggregate_function(self, name)
    }

    fn lookup_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        Engine::lookup_sql_functions(self, name)
    }

    fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        Engine::resolve_static_sql_function(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_static_sql_function_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        Engine::resolve_static_sql_function_match(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        Engine::resolve_table_function_overload_with_builtins(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }
}

pub(crate) fn routine_signature_types(def: &CreateFunction) -> Vec<String> {
    def.identity_params()
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect()
}

pub(crate) fn routine_returns_anonymous_record(def: &CreateFunction) -> bool {
    def.output_params().is_empty()
        && matches!(
            &def.returns,
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }
                if canonical_routine_type_name(type_name) == "record"
        )
}

pub(crate) struct StaticFunctionMatch {
    pub(crate) function: Arc<SQLUserFunction>,
    pub(crate) invocation: Box<RoutineInvocationBinding>,
    pub(super) argument_types: Vec<String>,
    pub(super) raw_exact_matches: usize,
    pub(super) exact_matches: usize,
    pub(super) preferred_matches: usize,
    pub(super) variadic_expansion: bool,
}

impl StaticFunctionMatch {
    pub(crate) fn binding(&self) -> FunctionBinding {
        FunctionBinding {
            name: self.function.def.name.clone(),
            argument_types: routine_signature_types(&self.function.def),
            builtin: false,
            dispatch: None,
            invocation: Some(self.invocation.clone()),
            resolution_error: None,
        }
    }
}

impl RankedFunctionMatch for StaticFunctionMatch {
    fn argument_types(&self) -> &[String] {
        &self.argument_types
    }

    fn raw_exact_matches(&self) -> usize {
        self.raw_exact_matches
    }

    fn exact_matches(&self) -> usize {
        self.exact_matches
    }

    fn preferred_matches(&self) -> usize {
        self.preferred_matches
    }

    fn is_variadic_expansion(&self) -> bool {
        self.variadic_expansion
    }
}

impl FunctionTypeResolver for Engine {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.has_registered_scalar_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(crate::sql::resolve_catalog_column_type(self, name))
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
        .map(|resolved| resolved.map(|resolved| resolved.return_type))
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        let Some(matched) = self.resolve_static_sql_routine_match(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            RoutineCallKind::Function,
        )?
        else {
            return Ok(None);
        };
        let function = &matched.function;
        Ok(Some(ResolvedFunctionOverload {
            binding: matched.binding(),
            return_type: static_function_return_type(
                self,
                name,
                &function.def,
                Some(&matched.invocation),
            )?,
            exact_matches: matched.exact_matches,
            known_arguments: argument_types.iter().flatten().count(),
            preferred_matches: matched.preferred_matches,
            precedes_pg_catalog: self.user_function_precedes_pg_catalog(&function.def.name),
        }))
    }

    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        if binding.builtin {
            return Ok(false);
        }
        let function = self
            .lookup_sql_functions(&binding.name)
            .and_then(|overloads| {
                overloads.into_iter().find(|function| {
                    !function.def.is_procedure
                        && routine_signature_types(&function.def) == binding.argument_types
                })
            });
        Ok(function.is_some_and(|function| !function.def.returns_set()))
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        combined_overloads::resolve(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
        .map(Some)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutineCallKind {
    Function,
    Procedure,
}

impl RoutineCallKind {
    fn is_procedure(self) -> bool {
        self == Self::Procedure
    }

    fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Procedure => "procedure",
        }
    }
}

impl Engine {
    pub(super) fn user_function_precedes_pg_catalog(&self, name: &str) -> bool {
        let Ok((Some(schema), _)) = RelationIdentity::parse_reference(name) else {
            return false;
        };
        let search_path = &self.session.state.read().search_path;
        let Some(user_position) = search_path.iter().position(|entry| entry == &schema) else {
            return false;
        };
        search_path
            .iter()
            .position(|entry| entry == "pg_catalog")
            .is_some_and(|catalog_position| user_position < catalog_position)
    }

    pub(crate) fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.resolve_static_sql_function_match(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
        .map(|matched| matched.map(|matched| matched.function))
    }

    pub(crate) fn resolve_static_sql_function_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        self.resolve_static_sql_routine_match(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            RoutineCallKind::Function,
        )
    }

    pub(crate) fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        combined_overloads::resolve_table(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
        .map(Some)
    }

    pub(crate) fn resolve_static_sql_routine_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        kind: RoutineCallKind,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        if let Some(binding) = binding {
            if binding.builtin {
                return Ok(None);
            }
            let function = self
                .lookup_sql_routine_candidates(&binding.name)
                .and_then(|overloads| {
                    overloads.into_iter().find(|function| {
                        function.def.is_procedure == kind.is_procedure()
                            && routine_signature_types(&function.def) == binding.argument_types
                    })
                })
                .ok_or_else(|| static_bound_routine_error(kind, binding))?;
            let matched = if let Some(invocation) = &binding.invocation {
                let invocation_is_explicit = matches!(
                    invocation.variadic_mode,
                    RoutineVariadicMode::Explicit { .. }
                );
                if invocation.argument_positions.len() != argument_types.len()
                    || invocation_is_explicit != explicit_variadic
                {
                    return Err(static_bound_routine_error(kind, binding));
                }
                StaticFunctionMatch {
                    function,
                    invocation: invocation.clone(),
                    argument_types: invocation.argument_targets.clone(),
                    raw_exact_matches: 0,
                    exact_matches: 0,
                    preferred_matches: 0,
                    variadic_expansion: matches!(
                        invocation.variadic_mode,
                        RoutineVariadicMode::Expanded { .. }
                    ),
                }
            } else {
                static_routine_match(
                    function,
                    argument_names,
                    argument_types,
                    explicit_variadic,
                    kind,
                )
                .map_err(|error| static_signature_error(kind, name, error))?
                .ok_or_else(|| static_bound_routine_error(kind, binding))?
            };
            ensure_routine_kind(name, argument_types, kind, &matched.function.def)?;
            return Ok(Some(matched));
        }
        let Some(overloads) = self.lookup_sql_routine_candidates(name) else {
            return Ok(None);
        };
        resolve_static_routine_overload(
            name,
            overloads,
            argument_names,
            argument_types,
            explicit_variadic,
            kind,
        )
        .map(Some)
    }
}

fn resolve_static_routine_overload(
    name: &str,
    overloads: Vec<Arc<SQLUserFunction>>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    kind: RoutineCallKind,
) -> Result<StaticFunctionMatch, SQLError> {
    let mut candidates = Vec::new();
    let mut match_error = None;
    for function in overloads {
        match static_routine_match(
            function,
            argument_names,
            argument_types,
            explicit_variadic,
            kind,
        ) {
            Ok(Some(candidate)) => candidates.push(candidate),
            Ok(None) => {}
            Err(error) => {
                match_error.get_or_insert(error);
            }
        }
    }
    retain_earliest_effective_signatures(&mut candidates);
    if candidates.is_empty() {
        if let Some(error) = match_error {
            return Err(static_signature_error(kind, name, error));
        }
        return Err(static_routine_resolution_error(
            kind,
            "42883",
            name,
            argument_types,
            "does not exist",
        ));
    }

    if !rank_function_matches(&mut candidates, argument_types) || candidates.len() != 1 {
        return Err(static_routine_resolution_error(
            kind,
            "42725",
            name,
            argument_types,
            "is not unique",
        ));
    }
    let matched = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved routine candidate disappeared".into()))?;
    ensure_routine_kind(name, argument_types, kind, &matched.function.def)?;
    Ok(matched)
}

pub(super) fn retain_earliest_effective_signatures(candidates: &mut Vec<StaticFunctionMatch>) {
    let mut visible = Vec::<(Vec<String>, String)>::new();
    candidates.retain(|candidate| {
        let schema = RelationIdentity::parse_reference(&candidate.function.def.name)
            .ok()
            .and_then(|(schema, _)| schema)
            .unwrap_or_default();
        if let Some((_, first_schema)) = visible
            .iter()
            .find(|(signature, _)| signature == &candidate.argument_types)
        {
            return first_schema == &schema;
        }
        visible.push((candidate.argument_types.clone(), schema));
        true
    });
}

fn static_routine_match(
    function: Arc<SQLUserFunction>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    kind: RoutineCallKind,
) -> Result<Option<StaticFunctionMatch>, RoutineSignatureMatchError> {
    let parameter_indices = routine_call_parameter_indices(&function.def, kind);
    let parameters = parameter_indices
        .iter()
        .map(|index| &function.def.params[*index])
        .collect::<Vec<_>>();
    let Some(matched) = match_static_function_signature(
        &parameters,
        argument_names,
        argument_types,
        explicit_variadic,
    )?
    else {
        return Ok(None);
    };
    let invocation = routine_invocation_binding(&function.def, &parameter_indices, &matched);
    Ok(Some(StaticFunctionMatch {
        function,
        argument_types: matched.argument_targets,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
        variadic_expansion: matches!(
            matched.variadic_mode,
            uqa_execution::RoutineVariadicMode::Pack
        ),
        invocation: Box::new(invocation),
    }))
}

pub(super) fn static_function_match(
    function: Arc<SQLUserFunction>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
) -> Result<Option<StaticFunctionMatch>, RoutineSignatureMatchError> {
    static_routine_match(
        function,
        argument_names,
        argument_types,
        explicit_variadic,
        RoutineCallKind::Function,
    )
}

fn match_static_function_signature(
    signature: &[&uqa_sql::ast::FunctionParam],
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
) -> Result<Option<MatchedRoutineSignature>, RoutineSignatureMatchError> {
    let parameters = signature
        .iter()
        .map(|parameter| RoutineParameterDescriptor {
            name: Some(parameter.name.clone()),
            type_name: canonical_routine_type_name(&parameter.type_name),
            has_default: parameter.default.is_some(),
            variadic: parameter.mode == FunctionParamMode::Variadic,
        })
        .collect::<Vec<_>>();
    match_routine_signature(
        &parameters,
        RoutineCallDescriptor {
            argument_names,
            argument_types,
            explicit_variadic,
        },
    )
}

fn routine_call_parameter_indices(def: &CreateFunction, kind: RoutineCallKind) -> Vec<usize> {
    def.params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            let participates = if kind == RoutineCallKind::Procedure && !def.is_procedure {
                true
            } else {
                match parameter.mode {
                    FunctionParamMode::In
                    | FunctionParamMode::InOut
                    | FunctionParamMode::Variadic => true,
                    FunctionParamMode::Out => def.is_procedure,
                    FunctionParamMode::Table => false,
                }
            };
            participates.then_some(index)
        })
        .collect()
}

fn routine_invocation_binding(
    def: &CreateFunction,
    parameter_indices: &[usize],
    matched: &MatchedRoutineSignature,
) -> RoutineInvocationBinding {
    let parameter_types = def
        .params
        .iter()
        .enumerate()
        .map(|(definition_index, parameter)| {
            parameter_indices
                .iter()
                .position(|index| *index == definition_index)
                .and_then(|call_index| matched.parameter_types.get(call_index).cloned())
                .or_else(|| matched.substitute_type_name(&parameter.type_name))
                .unwrap_or_else(|| canonical_routine_type_name(&parameter.type_name))
        })
        .collect::<Vec<_>>();
    let argument_positions = matched
        .argument_positions
        .iter()
        .map(|call_index| parameter_indices[*call_index])
        .collect();
    let variadic_mode = match &matched.variadic_plan {
        uqa_execution::RoutineVariadicPlan::Pack {
            parameter_index, ..
        } => RoutineVariadicMode::Expanded {
            parameter_index: parameter_indices[*parameter_index],
        },
        uqa_execution::RoutineVariadicPlan::PassThrough {
            parameter_index, ..
        } => RoutineVariadicMode::Explicit {
            parameter_index: parameter_indices[*parameter_index],
        },
        uqa_execution::RoutineVariadicPlan::None
        | uqa_execution::RoutineVariadicPlan::Default { .. } => RoutineVariadicMode::None,
    };
    let output_indices = def
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let return_type = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => matched
            .substitute_type_name(type_name)
            .or_else(|| Some(canonical_routine_type_name(type_name))),
        FunctionReturns::Table => Some("record".into()),
        FunctionReturns::None => match output_indices.as_slice() {
            [] => None,
            [index] => parameter_types.get(*index).cloned(),
            _ => Some("record".into()),
        },
    };
    RoutineInvocationBinding {
        argument_positions,
        argument_targets: matched.argument_targets.clone(),
        parameter_types,
        return_type,
        variadic_mode,
    }
}

pub(super) fn static_signature_error(
    kind: RoutineCallKind,
    name: &str,
    error: RoutineSignatureMatchError,
) -> SQLError {
    let sqlstate = error.sqlstate().to_string();
    let message = match error {
        RoutineSignatureMatchError::InvalidVariadicSignature { reason } => reason,
        RoutineSignatureMatchError::IndeterminatePolymorphicType { .. } => {
            format!(
                "could not determine polymorphic type for {} `{name}` because an input has type unknown",
                kind.name()
            )
        }
    };
    SQLError::Routine { sqlstate, message }
}

pub(super) fn static_function_return_type(
    engine: &Engine,
    name: &str,
    def: &CreateFunction,
    invocation: Option<&RoutineInvocationBinding>,
) -> Result<ColumnType, SQLError> {
    let invocation_return = invocation.and_then(|invocation| invocation.return_type.as_deref());
    let declared_return = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            Some(type_name.as_str())
        }
        FunctionReturns::Table | FunctionReturns::None => None,
    };
    if invocation_return
        .or(declared_return)
        .is_some_and(|type_name| canonical_routine_type_name(type_name) == "trigger")
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "trigger functions can only be called as triggers".into(),
        });
    }
    if matches!(def.returns, FunctionReturns::Table) || def.output_params().len() > 1 {
        return Ok(ColumnType::Record);
    }
    if let Some(type_name) = invocation_return {
        return crate::sql::resolve_catalog_column_type(engine, type_name)
            .or_else(|| ColumnType::from_sql_name(type_name).ok())
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "function `{name}` has unresolved return type `{type_name}`"
                ))
            });
    }
    let type_name = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => type_name,
        FunctionReturns::None => {
            let outputs = def.output_params();
            if outputs.len() > 1 {
                return Ok(ColumnType::Record);
            }
            &outputs
                .first()
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("function `{name}` does not return a value"))
                })?
                .type_name
        }
        FunctionReturns::Table => unreachable!("table result handled above"),
    };
    crate::sql::resolve_catalog_column_type(engine, type_name)
        .or_else(|| ColumnType::from_sql_name(type_name).ok())
        .ok_or_else(|| SQLError::TypeMismatch(format!("unknown type `{type_name}`")))
}

fn ensure_routine_kind(
    name: &str,
    argument_types: &[Option<ColumnType>],
    expected: RoutineCallKind,
    definition: &CreateFunction,
) -> Result<(), SQLError> {
    if definition.is_procedure == expected.is_procedure() {
        return Ok(());
    }
    let arguments = static_routine_argument_types(argument_types);
    let suffix = if definition.is_procedure {
        "is a procedure"
    } else {
        "is not a procedure"
    };
    Err(SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!("{name}({arguments}) {suffix}"),
    })
}

fn static_bound_routine_error(kind: RoutineCallKind, binding: &FunctionBinding) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "bound {} {}({}) does not exist",
            kind.name(),
            binding.name,
            binding.argument_types.join(", ")
        ),
    }
}

fn static_routine_resolution_error(
    kind: RoutineCallKind,
    sqlstate: &str,
    name: &str,
    argument_types: &[Option<ColumnType>],
    suffix: &str,
) -> SQLError {
    let arguments = static_routine_argument_types(argument_types);
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("{} {name}({arguments}) {suffix}", kind.name()),
    }
}

fn static_routine_argument_types(argument_types: &[Option<ColumnType>]) -> String {
    argument_types
        .iter()
        .map(|ty| {
            ty.as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::sql_name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn routine_kind(def: &CreateFunction) -> &'static str {
    if def.is_procedure {
        "procedure"
    } else {
        "function"
    }
}

pub(crate) fn routine_local_name(name: &str) -> Result<String, SQLError> {
    RelationIdentity::from_legacy_name(name)
        .map(|relation| relation.name)
        .map_err(|error| SQLError::Internal(format!("invalid routine name `{name}`: {error}")))
}
