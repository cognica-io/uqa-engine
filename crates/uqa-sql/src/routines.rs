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
mod defaults;
pub mod definition_output;
pub mod dependencies;
pub mod diagnostics;
pub mod inlining;
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
    match resolution.routine_analysis_body(function) {
        Ok(body) => Ok(Some(body)),
        Err(error) if matches!(error.primary(), SQLError::Internal(_)) => Err(error),
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
#[derive(Clone, Copy, Default)]
pub struct VisibleFunctionSetness {
    pub found: bool,
    /// `None` means either no scalar declaration or differing set-returning declarations.
    pub uniform: Option<bool>,
}

pub trait RoutineResolution: FunctionTypeResolver {
    fn has_registered_scalar_function(&self, _name: &str) -> bool {
        false
    }

    /// Native registries admit normalized lookup keys before allocation; external catalogs retain their existing scalar callback.
    fn has_registered_scalar_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let registered = self.has_registered_scalar_function(name);
        control.check()?;
        Ok(registered)
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

    /// Inspect a body for static effects and portal dependencies. A session may
    /// compile source independently here without creating its execution cache
    /// entry or issuing diagnostics for a body the query never reaches.
    fn routine_analysis_body(
        &self,
        function: &SQLUserFunction,
    ) -> Result<Arc<CompiledFunctionBody>, SQLError> {
        self.routine_body(function)
    }

    fn has_registered_table_function(&self, _name: &str) -> bool {
        false
    }

    /// Native registries admit normalized lookup keys before allocation; external catalogs retain their established boolean callback.
    fn has_registered_table_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let registered = self.has_registered_table_function(name);
        control.check()?;
        Ok(registered)
    }

    /// A registered function's stable output names, available without calling
    /// it. `None` means its result supplies the descriptor at execution.
    fn registered_table_function_columns(&self, _name: &str) -> Option<Vec<String>> {
        None
    }

    /// Native registries copy borrowed declarations under the request allowance; external catalogs retain their metadata callback.
    fn registered_table_function_columns_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        control.check()?;
        self.registered_table_function_columns(name)
            .map(|columns| {
                let mut output = uqa_core::memory::ProductionVec::new(*control);
                for column in columns {
                    output.push_produced(control.copy_text(&column)?)?;
                }
                Ok(output.finish()?)
            })
            .transpose()
    }

    fn has_registered_aggregate_function(&self, _name: &str) -> bool {
        false
    }

    fn has_registered_aggregate_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let registered = self.has_registered_aggregate_function(name);
        control.check()?;
        Ok(registered)
    }

    fn lookup_visible_sql_functions(
        &self,
        _name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(None)
    }

    /// Native catalogs inspect immutable buckets; external catalogs preserve their visible lookup.
    fn has_visible_sql_functions_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let found = self.lookup_visible_sql_functions(name)?.is_some();
        control.check()?;
        Ok(found)
    }

    /// Resolve candidate metadata without refreshing session-visible function state during analysis.
    fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        self.lookup_visible_sql_functions(name)
    }

    /// Observe the same visible, signature-shadowed candidates used by ordinary analysis without requiring a copied native candidate list.
    fn visible_function_setness_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<VisibleFunctionSetness, SQLError> {
        control.check()?;
        let candidates = self.lookup_visible_sql_functions_for_analysis(name)?;
        let mut result = VisibleFunctionSetness {
            found: candidates.is_some(),
            uniform: None,
        };
        let mut first = None;
        let mut mixed = false;
        for function in candidates
            .iter()
            .flatten()
            .filter(|function| !function.def.is_procedure)
        {
            control.check()?;
            let set = function.def.returns_set();
            match first {
                None => first = Some(set),
                Some(first) => mixed |= first != set,
            }
        }
        result.uniform = first.filter(|_| !mixed);
        Ok(result)
    }

    /// Observe visible set-returning declarations without constructing candidate snapshots in native catalogs. External adapters retain their existing analysis lookup.
    fn visible_function_returns_set_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let candidates = self.lookup_visible_sql_functions_for_analysis(name)?;
        let mut returns_set = false;
        for function in candidates.iter().flatten() {
            control.check()?;
            if !function.def.is_procedure && function.def.returns_set() {
                returns_set = true;
                break;
            }
        }
        Ok(returns_set)
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

    /// Select an already bound scalar routine. Native registries borrow their immutable bucket; custom catalogs may supply their existing candidate snapshot.
    fn select_bound_function_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        control.check()?;
        let candidates = self.lookup_bound_sql_functions_by_binding(binding);
        lifecycle::lookup::select_bound_function_with_control(
            candidates.iter().flatten(),
            binding,
            control,
        )
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

    /// Observe strictness after the same static function selection; native overload workspace retains the invoking allowance.
    fn resolve_static_sql_function_strictness_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: crate::type_resolution::RoutineCallDescriptor<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        let selected = self.resolve_static_sql_function(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
        )?;
        control.check()?;
        Ok(selected.map(|function| function.def.strict))
    }

    /// Select the same routine as static expression analysis and observe only its set-returning declaration. Native adapters retain admitted overload workspace; external adapters keep the existing selection callback.
    fn resolve_static_sql_function_setness_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        let result = self.resolve_static_sql_function(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )?;
        control.check()?;
        Ok(result.map(|function| function.def.returns_set()))
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

    /// Keep the selected invocation owned while callers derive table-result metadata.
    fn resolve_static_sql_function_match_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: crate::type_resolution::RoutineCallDescriptor<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<resolution::OwnedStaticFunctionMatch>, SQLError> {
        control.check()?;
        self.resolve_static_sql_function_match(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
        )?
        .map(|matched| {
            Ok(resolution::OwnedStaticFunctionMatch {
                function: matched.function,
                invocation: crate::plan::copy::copy_syntax_with_control(
                    matched.invocation.as_ref(),
                    control,
                )?,
            })
        })
        .transpose()
    }

    /// Construct the selected table-call binding without adding scalar output-type validation.
    fn resolve_static_sql_function_binding_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<FunctionBinding>>, SQLError> {
        control.check()?;
        self.resolve_static_sql_function_match(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )?
        .map(|matched| matched.binding_with_control(control))
        .transpose()
    }

    fn resolve_table_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: crate::type_resolution::RoutineCallDescriptor<'_>,
        builtins: &[BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        control.check()?;
        self.resolve_table_function_overload_with_builtins(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
            builtins,
        )?
        .map(|selected| selected.clone_with_control(control))
        .transpose()
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

/// Select the exact retained routine identity and declared signature without rebinding its name or executing its body.
pub fn selected_bound_function(
    routines: &dyn RoutineResolution,
    binding: &FunctionBinding,
) -> Option<std::sync::Arc<SQLUserFunction>> {
    selected_bound_function_with_control(
        routines,
        binding,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary selected routine lookup cannot be cancelled or limited")
}

pub fn selected_bound_function_with_control(
    routines: &dyn RoutineResolution,
    binding: &FunctionBinding,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
    control.check()?;
    if binding.builtin || binding.resolution_error.is_some() || binding.object_id.is_none() {
        return Ok(None);
    }
    routines.select_bound_function_with_control(binding, control)
}

pub(crate) fn routine_signature_matches_with_control(
    definition: &CreateFunction,
    types: &[String],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut parameters = definition.identity_params();
    for expected in types {
        let Some(parameter) = parameters.next() else {
            return Ok(false);
        };
        if *crate::type_resolution::canonical_routine_type_name_with_control(
            &parameter.type_name,
            control,
        )? != *expected
        {
            return Ok(false);
        }
    }
    Ok(parameters.next().is_none())
}

pub fn routine_signature_types(def: &CreateFunction) -> Vec<String> {
    def.identity_params()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect()
}

pub fn routine_returns_anonymous_record(def: &CreateFunction) -> bool {
    routine_returns_anonymous_record_with_control(
        def,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary routine return category")
}

pub struct StaticFunctionMatch {
    pub function: Arc<SQLUserFunction>,
    pub invocation: Box<RoutineInvocationBinding>,
    pub argument_types: Vec<String>,
    pub raw_exact_matches: usize,
    pub exact_matches: usize,
    pub preferred_matches: usize,
    pub variadic_expansion: bool,
    pub default_error: Option<crate::type_resolution::RoutineSignatureMatchError>,
}

impl StaticFunctionMatch {
    pub fn binding_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<FunctionBinding>, SQLError> {
        resolution::selected_binding(
            &self.function,
            crate::plan::copy::copy_syntax_with_control(self.invocation.as_ref(), control)?,
            control,
        )
    }

    pub fn binding(&self) -> FunctionBinding {
        FunctionBinding {
            object_id: self.function.def.object_id,
            name: self.function.def.name.clone(),
            argument_types: routine_signature_types(&self.function.def),
            builtin: false,
            dispatch: None,
            invocation: Some(self.invocation.clone()),
            value_binding: None,
            resolution_error: None,
            input_collation: None,
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
    routine_local_name_with_control(name, &uqa_core::memory::ProductionControl::uncontrolled()).map(
        |name| {
            name.into_uncontrolled()
                .expect("ordinary routine local name")
        },
    )
}

mod names_production;
pub use names_production::{
    routine_local_name_with_control, routine_returns_anonymous_record_with_control,
};

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
