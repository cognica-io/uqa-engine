//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL routine definitions and static signature lookup contracts.

pub mod attributes;
pub mod body_parameters;
pub mod body_validation;
pub mod compilation;
pub mod configuration;
pub mod declaration;
pub mod dependencies;
pub mod lifecycle;
pub mod merge_columns;
pub mod privilege_inquiry;
pub mod regclass;
pub mod registration;
pub mod resolution;
pub mod result_check;
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
use std::sync::{Arc, OnceLock};

/// A registered routine: the persistable definition and its body as the catalog keeps it.
#[derive(Clone)]
pub struct SQLUserFunction {
    pub def: CreateFunction,
    pub body: RoutineBody,
    version: OnceLock<u64>,
}

impl SQLUserFunction {
    #[must_use]
    pub const fn new(def: CreateFunction, body: RoutineBody) -> Self {
        Self {
            def,
            body,
            version: OnceLock::new(),
        }
    }

    /// The identity of this catalog tuple, preserved through reload and rollback. A legacy definition starts from its immutable object identity; publication assigns a new revision even when it keeps every SQL attribute unchanged.
    #[must_use]
    pub fn catalog_revision(&self) -> Option<[u8; 16]> {
        self.def.catalog_revision.or(self.def.object_id)
    }

    /// A fingerprint for the compiled-body cache, including the published catalog revision so an identical replacement recompiles its body. Exact catalog dependency checks use `catalog_revision` instead of this hash.
    pub fn definition_version(&self) -> Result<u64, SQLError> {
        if let Some(version) = self.version.get() {
            return Ok(*version);
        }
        let encoded = serde_json::to_vec(&self.def).map_err(|error| {
            SQLError::Internal(format!(
                "encode routine `{}` definition: {error}",
                self.def.name
            ))
        })?;
        // FNV-1a over the encoded definition.
        let version = encoded
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        Ok(*self.version.get_or_init(|| version))
    }
}

/// A routine body as the catalog keeps it. A SQL-standard body is bound when the routine is defined, as `PostgreSQL` stores `prosqlbody` as parse trees that name objects by OID. A body given as a string is compiled by each session that uses the routine, as the backend function cache compiles `prosrc`, so its names resolve when that session first needs them.
/// A routine body as the catalog keeps it.
#[derive(Clone)]
pub enum RoutineBody {
    /// A SQL-standard body, which the statement that defines the routine analyzes and binds.
    Bound(Arc<CompiledFunctionBody>),
    /// A body given as a string, which the catalog keeps as text, as `PostgreSQL` keeps `prosrc`: `CREATE FUNCTION` validates it only under `check_function_bodies`, and each session compiles it when it first calls the routine, so a body that no longer compiles reports its error then.
    Source,
}

/// The body a static analysis sees, or `None` when the body does not compile in this session. Such a routine cannot run in the session, and running it reports the compilation error, as `PostgreSQL` compiles a routine only when it runs; internal failures still propagate.
pub fn analyzable_routine_body(
    resolution: &(impl RoutineResolution + ?Sized),
    function: &SQLUserFunction,
) -> Result<Option<Arc<CompiledFunctionBody>>, SQLError> {
    match resolution.routine_body(function) {
        Ok(body) => Ok(Some(body)),
        Err(SQLError::Internal(message)) => Err(SQLError::Internal(message)),
        Err(_) => Ok(None),
    }
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

    /// The compiled body of a routine as this session executes it: a bound body as defined, or a source body as this session compiled it. A resolver without a session holds only bound bodies.
    fn routine_body(
        &self,
        function: &SQLUserFunction,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        match &function.body {
            RoutineBody::Bound(body) => Ok(Arc::clone(body)),
            RoutineBody::Source => Err(SQLError::Internal(format!(
                "routine `{}` has no session to compile its body",
                function.def.name
            ))),
        }
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

    /// Resolve an exact stored routine name without repeating visible-name lookup.
    fn lookup_bound_sql_functions(&self, _name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        None
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

pub mod anonymous_block;
pub mod call;
pub mod invocation;
