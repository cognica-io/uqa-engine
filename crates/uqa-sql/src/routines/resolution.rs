//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static routine signature matching, invocation binding, and declared return-type resolution.

mod catalog;
pub use catalog::{RoutineOverloadCatalog, RoutineTypeSnapshot};
mod combined_overloads;
mod production;
pub use production::search_path_precedes_pg_catalog;

use super::{
    declaration::RoutineTypeCatalog, routine_signature_types, SQLUserFunction, StaticFunctionMatch,
};
use crate::type_resolution::{
    canonical_routine_type_name, rank_function_matches, BuiltinFunctionOverload,
    FunctionTypeResolver, ResolvedFunctionOverload, RoutineCallDescriptor,
    RoutineSignatureMatchError,
};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBinding, RoutineVariadicMode},
    SQLError,
};
use std::sync::Arc;
use uqa_core::RelationIdentity;

pub struct RoutineOverloadContext<'a> {
    pub catalog: &'a dyn RoutineOverloadCatalog,
}

impl FunctionTypeResolver for RoutineOverloadContext<'_> {
    fn format_type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        self.catalog.format_type(ty)
    }

    fn format_type_name_with_control(
        &self,
        ty: &ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        self.catalog.format_type_with_control(ty, control)
    }

    fn composite_types(&self) -> Option<&dyn crate::expr::composites::CompositeTypeCatalog> {
        self.catalog.composite_types()
    }

    fn has_untyped_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.catalog
            .has_registered_scalar_function_with_control(name, control)
    }

    fn has_untyped_function(&self, name: &str) -> bool {
        self.catalog.has_registered_scalar_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.catalog
            .resolve_catalog_column_type_name(name)
            .map(Some)
    }

    fn resolve_type_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        self.catalog
            .resolve_catalog_column_type_name_with_control(name, control)
            .map(Some)
    }

    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError> {
        self.catalog.require_type_usage(ty)
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

    fn resolve_function_type_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        self.resolve_function_overload_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            control,
        )?
        .map(|result| {
            result
                .return_type
                .clone_with_control(control)
                .map_err(Into::into)
        })
        .transpose()
    }

    fn resolve_function_overload_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        production::selection::resolve(
            self,
            name,
            binding,
            RoutineCallDescriptor {
                argument_names,
                argument_types,
                explicit_variadic,
            },
            control,
        )
    }

    fn resolve_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        production::combined::resolve(
            self,
            name,
            binding,
            RoutineCallDescriptor {
                argument_names,
                argument_types,
                explicit_variadic,
            },
            builtins,
            control,
        )
        .map(Some)
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.resolve_function_overload_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .map(|result| {
            result.map(|result| {
                result
                    .into_uncontrolled()
                    .expect("ordinary routine resolution")
            })
        })
    }

    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        if binding.builtin {
            return Ok(false);
        }
        let function = self
            .catalog
            .lookup_bound_sql_functions_by_binding(binding)
            .and_then(|overloads| {
                overloads.into_iter().find(|function| {
                    !function.def.is_procedure
                        && routine_signature_types(&function.def) == binding.argument_types
                })
            });
        Ok(function.is_some_and(|function| !function.def.returns_set()))
    }

    fn is_scalar_function_binding_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        if binding.builtin {
            return Ok(false);
        }
        Ok(self
            .catalog
            .select_bound_routine_with_control(binding, RoutineCallKind::Function, control)?
            .is_some_and(|function| !function.def.returns_set()))
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
pub enum RoutineCallKind {
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

impl RoutineOverloadContext<'_> {
    pub fn resolve_static_sql_function(
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

    pub fn resolve_static_sql_function_match(
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

    pub fn resolve_table_function_overload_with_builtins(
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

    pub fn resolve_static_sql_routine_match(
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
                .catalog
                .lookup_bound_sql_routine_candidates_by_binding(binding)
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
                    default_error: None,
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
                    &self.catalog.routine_type_snapshot(),
                    function,
                    argument_names,
                    argument_types,
                    explicit_variadic,
                    kind,
                )
                .map_err(|error| static_signature_error(kind, name, error))?
                .ok_or_else(|| static_bound_routine_error(kind, binding))?
            };
            ensure_routine_kind(
                name,
                argument_names,
                argument_types,
                kind,
                &matched.function.def,
            )?;
            check_selected_default_types(self.catalog, kind, name, &matched)?;
            return Ok(Some(matched));
        }
        let Some(overloads) = self.catalog.lookup_sql_routine_candidates(name)? else {
            return Ok(None);
        };
        resolve_static_routine_overload(
            self.catalog,
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
    catalog: &dyn RoutineOverloadCatalog,
    name: &str,
    overloads: Vec<Arc<SQLUserFunction>>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    kind: RoutineCallKind,
) -> Result<StaticFunctionMatch, SQLError> {
    let snapshot = catalog.routine_type_snapshot();
    let mut candidates = Vec::new();
    let mut match_error = None;
    for function in overloads {
        match static_routine_match(
            &snapshot,
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
    ensure_routine_kind(
        name,
        argument_names,
        argument_types,
        kind,
        &matched.function.def,
    )?;
    check_selected_default_types(catalog, kind, name, &matched)?;
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
    catalog: &RoutineTypeSnapshot,
    function: Arc<SQLUserFunction>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    kind: RoutineCallKind,
) -> Result<Option<StaticFunctionMatch>, RoutineSignatureMatchError> {
    production::match_ordinary(
        catalog,
        function,
        RoutineCallDescriptor {
            argument_names,
            argument_types,
            explicit_variadic,
        },
        kind,
    )
}

pub(crate) fn static_signature_error(
    _kind: RoutineCallKind,
    _name: &str,
    error: RoutineSignatureMatchError,
) -> SQLError {
    production::diagnostics::signature(&error, &uqa_core::memory::ProductionControl::uncontrolled())
        .expect("ordinary signature diagnostic")
}

fn ensure_routine_kind(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    expected: RoutineCallKind,
    definition: &CreateFunction,
) -> Result<(), SQLError> {
    production::diagnostics::kind(
        name,
        RoutineCallDescriptor {
            argument_names,
            argument_types,
            explicit_variadic: false,
        },
        expected,
        definition,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

fn static_bound_routine_error(kind: RoutineCallKind, binding: &FunctionBinding) -> SQLError {
    production::diagnostics::bound(
        kind,
        binding,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary bound routine diagnostic")
}

fn static_routine_resolution_error(
    kind: RoutineCallKind,
    sqlstate: &str,
    name: &str,
    argument_types: &[Option<ColumnType>],
    suffix: &str,
) -> SQLError {
    production::diagnostics::resolution(
        kind,
        sqlstate,
        name,
        RoutineCallDescriptor {
            argument_names: &[],
            argument_types,
            explicit_variadic: false,
        },
        suffix,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary routine resolution diagnostic")
}

/// Defaults do not exclude or rank an overload. Their polymorphic consistency is
/// checked only after the ordinary supplied-argument rules choose one candidate.
pub(super) fn check_selected_default_types(
    catalog: &dyn RoutineTypeCatalog,
    _kind: RoutineCallKind,
    _name: &str,
    matched: &StaticFunctionMatch,
) -> Result<(), SQLError> {
    if let Some(error) = &matched.default_error {
        return Err(default_type_error(catalog, error)?);
    }
    // An actual anyarray supplies no element identity. Its sole-input exception
    // permits an anyarray result, but cannot specialize an anyelement result.
    if matched
        .invocation
        .parameter_types
        .iter()
        .any(|ty| ty == "anyarray")
        && matched.invocation.return_type.as_deref().is_some_and(|ty| {
            matches!(
                canonical_routine_type_name(ty).as_str(),
                "anyelement" | "anynonarray" | "anyenum"
            )
        })
    {
        return Err(default_type_error(
            catalog,
            &RoutineSignatureMatchError::IndeterminateArrayElement,
        )?);
    }
    Ok(())
}

fn default_type_error(
    catalog: &dyn RoutineTypeCatalog,
    error: &RoutineSignatureMatchError,
) -> Result<SQLError, SQLError> {
    production::diagnostics::default_type(
        catalog,
        error,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}
