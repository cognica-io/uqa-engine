//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL routine definitions and static signature lookup contracts.

pub mod compilation;
pub mod configuration;
pub mod declaration;
pub mod dependencies;
pub mod lifecycle;
pub mod merge_columns;
pub mod regclass;
pub mod registration;
pub mod resolution;
pub mod security;

use crate::ast::{
    ColumnType, CreateFunction, FunctionBinding, FunctionReturns, RoutineInvocationBinding,
};
use crate::plan::UnifiedPlan;
use crate::type_resolution::{
    canonical_routine_type_name, BuiltinFunctionOverload, FunctionTypeResolver,
    RankedFunctionMatch, ResolvedFunctionOverload,
};
use crate::SQLError;
use std::sync::Arc;

/// A registered routine: the persistable definition plus its
/// pre-compiled body.
#[derive(Clone)]
pub struct SQLUserFunction {
    pub def: CreateFunction,
    pub compiled: CompiledFunctionBody,
}

/// Executable form of a routine body.
// The project naming convention spells the acronym as `SQL`.
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone)]
pub enum CompiledFunctionBody {
    PLpgSQL(crate::plpgsql::PLpgSQLFunction),
    SQL(Vec<UnifiedPlan>),
}
pub fn is_routine_namespace_lookup_error(error: &SQLError) -> bool {
    matches!(
        error,
        SQLError::Routine { sqlstate, message }
            if sqlstate == "3F000"
                || (sqlstate == "42501"
                    && message.starts_with("permission denied for schema "))
    )
}

/// Function-catalog operations required by static query binding. The interface deliberately excludes storage, transaction, locking, and execution services so the binder can run against a deterministic catalog fixture.
pub trait RoutineResolution: FunctionTypeResolver {
    fn has_registered_scalar_function(&self, _name: &str) -> bool {
        false
    }

    fn has_registered_table_function(&self, _name: &str) -> bool {
        false
    }

    fn has_registered_aggregate_function(&self, _name: &str) -> bool {
        false
    }

    fn lookup_visible_sql_functions(
        &self,
        _name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(None)
    }

    /// Resolve candidate metadata without refreshing session-visible function state during analysis.
    fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        self.lookup_visible_sql_functions(name)
    }

    fn lookup_bound_sql_functions_by_binding(
        &self,
        _binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
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

pub fn routine_signature_types(def: &CreateFunction) -> Vec<String> {
    def.identity_params()
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect()
}

pub fn routine_returns_anonymous_record(def: &CreateFunction) -> bool {
    def.output_params().is_empty()
        && matches!(
            &def.returns,
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }
                if canonical_routine_type_name(type_name) == "record"
        )
}

pub struct StaticFunctionMatch {
    pub function: Arc<SQLUserFunction>,
    pub invocation: Box<RoutineInvocationBinding>,
    pub argument_types: Vec<String>,
    pub raw_exact_matches: usize,
    pub exact_matches: usize,
    pub preferred_matches: usize,
    pub variadic_expansion: bool,
}

impl StaticFunctionMatch {
    pub fn binding(&self) -> FunctionBinding {
        FunctionBinding {
            object_id: self.function.def.object_id,
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

pub fn builtin_routine_support_oid(name: &str) -> Option<i64> {
    Some(match name.strip_prefix("pg_catalog.").unwrap_or(name) {
        "textlike_support" => 1023,
        "texticregexeq_support" => 1024,
        "texticlike_support" => 1025,
        "network_subset_support" => 1173,
        "textregexeq_support" => 1364,
        "varchar_support" => 3097,
        "numeric_support" => 3157,
        _ => return None,
    })
}

pub fn function_binding_matches(binding: &FunctionBinding, target: &FunctionBinding) -> bool {
    if binding.builtin || target.builtin {
        return false;
    }
    match (binding.object_id, target.object_id) {
        (Some(binding), Some(target)) => binding == target,
        (None, None) => {
            binding.name == target.name && binding.argument_types == target.argument_types
        }
        _ => false,
    }
}

pub fn routine_local_name(name: &str) -> Result<String, SQLError> {
    uqa_core::RelationIdentity::from_legacy_name(name)
        .map(|relation| relation.name)
        .map_err(|error| SQLError::Internal(format!("invalid routine name `{name}`: {error}")))
}

pub fn routine_kind(def: &CreateFunction) -> &'static str {
    if def.is_procedure {
        "procedure"
    } else {
        "function"
    }
}
