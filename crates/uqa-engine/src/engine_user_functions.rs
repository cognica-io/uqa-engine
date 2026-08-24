//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry for user-defined SQL / `PL/pgSQL` routines (`CREATE
//! FUNCTION` / `CREATE PROCEDURE`). Definitions persist to catalog
//! metadata like views and sequences; the compiled body is rebuilt
//! from the definition at registration and restore time.

mod combined_overloads;

use std::collections::BTreeMap;

pub(crate) use uqa_execution::canonical_routine_type_name;
use uqa_execution::{
    match_function_signature, rank_function_matches, BuiltinFunctionOverload,
    FunctionParameterDescriptor, FunctionTypeResolver, MatchedFunctionSignature,
    RankedFunctionMatch, ResolvedFunctionOverload, ScalarExpr,
};
use uqa_planner::UnifiedPlan;
use uqa_sql::ast::{
    ColumnType, CreateFunction, DropFunctionItem, DropFunctionStmt, FunctionBinding, FunctionBody,
    FunctionReturns, RoutineColumnTypeReference, Statement,
};
use uqa_sql::SQLError;

use super::{
    Arc, CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
    FUNCTIONS_METADATA_KEY,
};

/// A registered routine: the persistable definition plus its
/// pre-compiled body.
pub(crate) struct SQLUserFunction {
    pub def: CreateFunction,
    pub compiled: CompiledFunctionBody,
}

/// Executable form of a routine body.
// The project naming convention spells the acronym as `SQL`.
#[allow(clippy::upper_case_acronyms)]
pub(crate) enum CompiledFunctionBody {
    PLpgSQL(uqa_sql::plpgsql::PLpgSQLFunction),
    SQL(Vec<UnifiedPlan>),
}

pub(crate) fn routine_signature_types(def: &CreateFunction) -> Vec<String> {
    def.signature_params()
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect()
}

struct StaticFunctionMatch {
    function: Arc<SQLUserFunction>,
    argument_types: Vec<String>,
    raw_exact_matches: usize,
    exact_matches: usize,
    preferred_matches: usize,
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
    ) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_function_overload(name, binding, argument_names, argument_types)
            .map(|resolved| resolved.map(|resolved| resolved.return_type))
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        let Some(matched) = self.resolve_static_sql_routine_match(
            name,
            binding,
            argument_names,
            argument_types,
            RoutineCallKind::Function,
        )?
        else {
            return Ok(None);
        };
        let function = &matched.function;
        Ok(Some(ResolvedFunctionOverload {
            binding: FunctionBinding {
                name: function.def.name.clone(),
                argument_types: routine_signature_types(&function.def),
                builtin: false,
            },
            return_type: static_function_return_type(name, &function.def)?,
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
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        combined_overloads::resolve(
            self,
            name,
            binding,
            argument_names,
            argument_types,
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
    fn user_function_precedes_pg_catalog(&self, name: &str) -> bool {
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
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.resolve_static_sql_routine(
            name,
            binding,
            argument_names,
            argument_types,
            RoutineCallKind::Function,
        )
    }

    /// Resolve a function-expression or `CALL` candidate with shared `PostgreSQL` visibility and overload-ranking rules.
    pub(crate) fn resolve_static_sql_routine(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        kind: RoutineCallKind,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.resolve_static_sql_routine_match(name, binding, argument_names, argument_types, kind)
            .map(|matched| matched.map(|matched| matched.function))
    }

    fn resolve_static_sql_routine_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
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
            // The binding already fixes identity; re-match only named/default structure. Unknown structural types yield zero ranking scores, which are unused on this return path.
            let bound_argument_types = vec![None; argument_types.len()];
            let matched =
                static_routine_match(function, argument_names, &bound_argument_types, kind)
                    .ok_or_else(|| static_bound_routine_error(kind, binding))?;
            ensure_routine_kind(name, argument_types, kind, &matched.function.def)?;
            return Ok(Some(matched));
        }
        let Some(overloads) = self.lookup_sql_routine_candidates(name) else {
            return Ok(None);
        };
        resolve_static_routine_overload(name, overloads, argument_names, argument_types, kind)
            .map(Some)
    }
}

fn resolve_static_routine_overload(
    name: &str,
    overloads: Vec<Arc<SQLUserFunction>>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    kind: RoutineCallKind,
) -> Result<StaticFunctionMatch, SQLError> {
    let mut candidates = overloads
        .into_iter()
        .filter_map(|function| static_routine_match(function, argument_names, argument_types, kind))
        .collect::<Vec<_>>();
    retain_earliest_effective_signatures(&mut candidates);
    if candidates.is_empty() {
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

fn retain_earliest_effective_signatures(candidates: &mut Vec<StaticFunctionMatch>) {
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
    kind: RoutineCallKind,
) -> Option<StaticFunctionMatch> {
    let parameters = if kind == RoutineCallKind::Procedure && !function.def.is_procedure {
        function.def.params.iter().collect()
    } else {
        function.def.signature_params()
    };
    let matched = match_static_function_signature(&parameters, argument_names, argument_types)?;
    Some(StaticFunctionMatch {
        function,
        argument_types: matched.argument_types,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
    })
}

fn static_function_match(
    function: Arc<SQLUserFunction>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<StaticFunctionMatch> {
    let signature = function.def.signature_params();
    let matched = match_static_function_signature(&signature, argument_names, argument_types)?;
    Some(StaticFunctionMatch {
        function,
        argument_types: matched.argument_types,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
    })
}

fn match_static_function_signature(
    signature: &[&uqa_sql::ast::FunctionParam],
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<MatchedFunctionSignature> {
    let parameters = signature
        .iter()
        .map(|parameter| FunctionParameterDescriptor {
            name: Some(parameter.name.clone()),
            type_name: canonical_routine_type_name(&parameter.type_name),
            has_default: parameter.default.is_some(),
        })
        .collect::<Vec<_>>();
    match_function_signature(&parameters, argument_names, argument_types)
}

fn static_function_return_type(name: &str, def: &CreateFunction) -> Result<ColumnType, SQLError> {
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
        FunctionReturns::Table => {
            return Err(SQLError::TypeMismatch(format!(
                "function `{name}` returns a record, not one scalar value"
            )));
        }
    };
    ColumnType::from_sql_name(type_name)
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

fn routine_kind(def: &CreateFunction) -> &'static str {
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

fn routine_signature_label(name: &str, types: &[String]) -> String {
    format!("{name}({})", types.join(", "))
}

fn wrong_routine_kind_error(
    name: &str,
    types: &[String],
    actual_is_procedure: bool,
    expected_kind: &str,
) -> SQLError {
    let actual_kind = if actual_is_procedure {
        "procedure"
    } else {
        "function"
    };
    SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!(
            "{} is a {actual_kind}, not a {expected_kind}",
            routine_signature_label(name, types)
        ),
    }
}

fn resolve_routine_type_references(
    engine: &Engine,
    def: &mut CreateFunction,
) -> Result<(), SQLError> {
    for parameter in &mut def.params {
        parameter.type_name = resolve_routine_type_name_with_reference(
            engine,
            &parameter.type_name,
            &["record", "anyarray"],
            parameter.type_reference.as_ref(),
        )?;
        parameter.type_reference = None;
    }
    match &mut def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            *type_name = resolve_routine_type_name_with_reference(
                engine,
                type_name,
                &["void", "record", "anyarray"],
                def.return_type_reference.as_ref(),
            )?;
        }
        FunctionReturns::None | FunctionReturns::Table => {}
    }
    def.return_type_reference = None;
    Ok(())
}

fn resolve_routine_type_name_with_reference(
    engine: &Engine,
    type_name: &str,
    allowed_pseudo_types: &[&str],
    structured_reference: Option<&RoutineColumnTypeReference>,
) -> Result<String, SQLError> {
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
        let columns = engine
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
            let mut resolved = canonical;
            for _ in 0..array_dimensions {
                resolved.push_str("[]");
            }
            return Ok(resolved);
        }
        ColumnType::from_sql_name(base).map_err(|error| match error {
            SQLError::Unsupported(_) => {
                SQLError::Unsupported(format!("routine type `{type_name}` is not implemented"))
            }
            other => other,
        })?
    };
    let mut resolved = resolved;
    for _ in 0..array_dimensions {
        resolved = ColumnType::Array(Box::new(resolved));
    }
    Ok(resolved.sql_name())
}

fn resolve_plpgsql_datum_types(
    engine: &Engine,
    function: &mut uqa_sql::plpgsql::PLpgSQLFunction,
) -> Result<(), SQLError> {
    for datum in &mut function.datums {
        let uqa_sql::plpgsql::PLpgSQLDatum::Var(variable) = datum else {
            continue;
        };
        variable.type_name = resolve_routine_type_name_with_reference(
            engine,
            &variable.type_name,
            &["record", "anyarray", "refcursor"],
            variable.type_reference.as_ref(),
        )?;
        variable.type_reference = None;
    }
    Ok(())
}

/// Compile a routine body per its language. Shared by DDL
/// registration and restore-from-catalog.
pub(crate) fn compile_function_body(
    engine: &Engine,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    match def.language.as_str() {
        "plpgsql" => {
            if matches!(def.body, FunctionBody::Statements(_)) {
                return Err(SQLError::Unsupported(
                    "LANGUAGE plpgsql with a SQL-standard body".into(),
                ));
            }
            let mut function = uqa_sql::plpgsql::parse_function(def)?;
            resolve_plpgsql_datum_types(engine, &mut function)?;
            Ok(CompiledFunctionBody::PLpgSQL(function))
        }
        "sql" => {
            let statements = match &def.body {
                FunctionBody::Source(source) => uqa_sql::compile(source)?,
                FunctionBody::Statements(statements) => statements.clone(),
            };
            Ok(CompiledFunctionBody::SQL(compile_sql_routine_plans(
                engine, def, statements,
            )?))
        }
        other => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{other}\" does not exist"),
        }),
    }
}

fn compile_sql_routine_plans(
    engine: &Engine,
    def: &CreateFunction,
    statements: Vec<Statement>,
) -> Result<Vec<UnifiedPlan>, SQLError> {
    let local_name = routine_local_name(&def.name)?;
    let parameter_names: Vec<String> = def
        .signature_params()
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    statements
        .into_iter()
        .map(|statement| {
            let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
                engine.has_registered_aggregate_function(name)
            });
            plan.rewrite_scalar_expressions(&mut |expression| {
                let parameter = match expression {
                    ScalarExpr::Column(name) => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == name),
                    ScalarExpr::QualifiedColumn {
                        qualifier, column, ..
                    } if qualifier == &local_name => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == column),
                    _ => None,
                };
                if let Some(position) = parameter {
                    *expression = ScalarExpr::Param(position + 1);
                }
            });
            crate::sql::optimize_engine_plan(engine, plan)
        })
        .collect()
}

impl Engine {
    /// Register (or replace) a user-defined routine. Applies the
    /// `PostgreSQL` conflict rules for `(schema, name, argument types)`
    /// collisions and persists the updated overload set.
    pub(crate) fn register_sql_function(&self, mut def: CreateFunction) -> Result<(), SQLError> {
        let requested_name = def.name.clone();
        def.name = self
            .try_relation_name_for_create(&requested_name)
            .map_err(|error| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: error,
            })?;
        resolve_routine_type_references(self, &mut def)?;
        let compiled = compile_function_body(self, &def)?;
        let name = def.name.clone();
        let signature = routine_signature_types(&def);
        let kind = routine_kind(&def);
        let mut registry = self.durable.sql_user_functions.write();
        let mut next = registry.clone();
        {
            let overloads = next.entry(name.clone()).or_default();
            if let Some(pos) = overloads
                .iter()
                .position(|function| routine_signature_types(&function.def) == signature)
            {
                let existing = &overloads[pos].def;
                if !def.or_replace {
                    return Err(SQLError::Routine {
                        sqlstate: "42723".into(),
                        message: format!(
                            "{kind} \"{requested_name}\" already exists with same argument types"
                        ),
                    });
                }
                if existing.is_procedure != def.is_procedure {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: "cannot change routine kind".into(),
                    });
                }
                if !same_return_shape(existing, &def) {
                    return Err(SQLError::Routine {
                        sqlstate: "42P13".into(),
                        message: "cannot change return type of existing function".into(),
                    });
                }
                overloads[pos] = Arc::new(SQLUserFunction { def, compiled });
            } else {
                overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            }
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
        }
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Drop routines per a `DROP FUNCTION` / `DROP PROCEDURE`
    /// statement, mirroring `PostgreSQL`'s resolution and error
    /// texts. `IF EXISTS` misses emit a notice instead of failing.
    pub(crate) fn drop_sql_functions(&self, stmt: &DropFunctionStmt) -> Result<(), SQLError> {
        if stmt.cascade {
            return Err(SQLError::Unsupported(
                "DROP FUNCTION/PROCEDURE CASCADE is not supported".into(),
            ));
        }
        let kind = if stmt.is_procedure {
            "procedure"
        } else {
            "function"
        };
        let mut registry = self.durable.sql_user_functions.write();
        let mut next = registry.clone();
        let mut dropped = false;
        let mut notices = Vec::new();
        for item in &stmt.items {
            let target =
                self.resolve_sql_function_drop_target(&next, item, stmt.is_procedure, kind)?;
            if let Some((key, position)) = target {
                let list = next.get_mut(&key).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {kind} registry entry `{key}` disappeared before DROP"
                    ))
                })?;
                let signature = routine_signature_types(&list[position].def);
                self.ensure_no_generated_function_dependencies(&key, &signature)?;
                list.remove(position);
                if list.is_empty() {
                    next.remove(&key);
                }
                dropped = true;
            } else {
                let spelled = match &item.arg_types {
                    Some(types) => format!("{}({})", item.name, types.join(", ")),
                    None => format!("{}()", item.name),
                };
                if stmt.if_exists {
                    notices.push((
                        "NOTICE",
                        format!("{kind} {spelled} does not exist, skipping"),
                    ));
                    continue;
                }
                let described = match &item.arg_types {
                    Some(_) => format!("{kind} {spelled} does not exist"),
                    None => format!("could not find a {kind} named \"{}\"", item.name),
                };
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: described,
                });
            }
        }
        if dropped {
            self.persist_sql_functions_snapshot(&next)?;
            *registry = next;
            drop(registry);
            self.note_catalog_registry_changed();
        }
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        Ok(())
    }

    fn generated_function_dependents(
        &self,
        name: &str,
        argument_types: &[String],
    ) -> Result<Vec<String>, SQLError> {
        let mut dependents = Vec::new();
        for table in self.table_names().map_err(|error| {
            SQLError::Internal(format!("read generated function dependencies: {error}"))
        })? {
            let columns = self
                .try_describe_table(&table)
                .map_err(|error| {
                    SQLError::Internal(format!("read generated function dependencies: {error}"))
                })?
                .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
            for column in columns {
                let Some(generated) = column.generated else {
                    continue;
                };
                if generated.function_dependencies.iter().any(|dependency| {
                    dependency.name == name && dependency.argument_types == argument_types
                }) {
                    dependents.push(format!("{table}.{}", column.name));
                }
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    fn ensure_no_generated_function_dependencies(
        &self,
        name: &str,
        argument_types: &[String],
    ) -> Result<(), SQLError> {
        let dependents = self.generated_function_dependents(name, argument_types)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop function {} because generated column(s) `{}` depend on it",
                routine_signature_label(name, argument_types),
                dependents.join("`, `")
            ),
        })
    }

    fn resolve_sql_function_drop_target(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        item: &DropFunctionItem,
        is_procedure: bool,
        expected_kind: &str,
    ) -> Result<Option<(String, usize)>, SQLError> {
        let requested_types = item.arg_types.as_ref().map(|types| {
            types
                .iter()
                .map(|type_name| canonical_routine_type_name(type_name))
                .collect::<Vec<_>>()
        });
        for key in self.routine_lookup_keys(&item.name)? {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            if let Some(types) = requested_types.as_ref() {
                let Some((position, function)) = overloads
                    .iter()
                    .enumerate()
                    .find(|(_, function)| routine_signature_types(&function.def) == *types)
                else {
                    continue;
                };
                if function.def.is_procedure != is_procedure {
                    return Err(wrong_routine_kind_error(
                        &function.def.name,
                        types,
                        function.def.is_procedure,
                        expected_kind,
                    ));
                }
                return Ok(Some((key, position)));
            }

            let positions = overloads
                .iter()
                .enumerate()
                .filter(|(_, function)| function.def.is_procedure == is_procedure)
                .map(|(position, _)| position)
                .collect::<Vec<_>>();
            match positions.as_slice() {
                [] => {
                    if let Some(function) = overloads.first() {
                        return Err(wrong_routine_kind_error(
                            &function.def.name,
                            &routine_signature_types(&function.def),
                            function.def.is_procedure,
                            expected_kind,
                        ));
                    }
                }
                [position] => return Ok(Some((key, *position))),
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "42725".into(),
                        message: format!("{expected_kind} name \"{}\" is not unique", item.name),
                    });
                }
            }
        }
        Ok(None)
    }

    fn routine_lookup_keys(&self, name: &str) -> Result<Vec<String>, SQLError> {
        let (schema, local_name) =
            RelationIdentity::parse_reference(name).map_err(|error| SQLError::Routine {
                sqlstate: "42602".into(),
                message: format!("invalid routine name `{name}`: {error}"),
            })?;
        if let Some(schema) = schema {
            return Ok(vec![
                RelationIdentity::new(schema, local_name).qualified_name()
            ]);
        }
        Ok(self
            .session
            .state
            .read()
            .search_path
            .iter()
            .map(|schema| RelationIdentity::new(schema, &local_name).qualified_name())
            .collect())
    }

    /// Visible overload set for `name`. Identical signatures in later
    /// `search_path` schemas are shadowed while distinct signatures remain
    /// candidates, matching `PostgreSQL`'s routine lookup rules.
    pub(crate) fn lookup_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = self.routine_lookup_keys(name).ok()?;
        let registry = self.durable.sql_user_functions.read();
        let mut visible = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for key in keys {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            for function in overloads {
                let identity = (
                    routine_signature_types(&function.def),
                    function.def.is_procedure,
                );
                if seen.insert(identity) {
                    visible.push(function.clone());
                }
            }
        }
        (!visible.is_empty()).then_some(visible)
    }

    /// Call-resolution candidates before search-path shadowing. Named notation can make a later identical declared signature visible when an earlier routine uses different parameter names, so structural matching must happen first.
    fn lookup_sql_routine_candidates(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = self.routine_lookup_keys(name).ok()?;
        let registry = self.durable.sql_user_functions.read();
        let candidates = keys
            .into_iter()
            .filter_map(|key| registry.get(&key))
            .flat_map(|overloads| overloads.iter().cloned())
            .collect::<Vec<_>>();
        (!candidates.is_empty()).then_some(candidates)
    }

    /// Every registered routine, sorted by qualified name then signature. Feeds
    /// `pg_catalog.pg_proc` / `information_schema.routines`.
    pub(crate) fn list_sql_functions(&self) -> Vec<Arc<SQLUserFunction>> {
        let registry = self.durable.sql_user_functions.read();
        let mut out: Vec<Arc<SQLUserFunction>> = Vec::new();
        for overloads in registry.values() {
            let mut sorted = overloads.clone();
            sorted.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
            out.extend(sorted);
        }
        out
    }

    /// Current nesting cap for user-defined routine calls.
    pub fn sql_function_depth_limit(&self) -> usize {
        self.runtime
            .function_depth_limit
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adjust the nesting cap for user-defined routine calls
    /// (minimum 1). Mirrors `PostgreSQL`'s `max_stack_depth` role for
    /// recursive functions.
    pub fn set_sql_function_depth_limit(&self, limit: usize) {
        self.runtime
            .function_depth_limit
            .store(limit.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Queue a notice (`RAISE NOTICE` / `WARNING` / ...).
    pub(crate) fn push_sql_notice(&self, level: &str, message: &str) {
        self.runtime
            .notices
            .lock()
            .push((level.to_string(), message.to_string()));
    }

    /// Drain queued notices as `(level, message)` pairs in emission
    /// order.
    pub fn take_sql_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.runtime.notices.lock())
    }

    fn persist_sql_functions_snapshot(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let defs: BTreeMap<String, Vec<CreateFunction>> = registry
            .iter()
            .map(|(name, overloads)| {
                (
                    name.clone(),
                    overloads
                        .iter()
                        .map(|function| function.def.clone())
                        .collect(),
                )
            })
            .collect();
        let json = serde_json::to_string(&defs)
            .map_err(|err| SQLError::Internal(format!("serialize function catalog: {err}")))?;
        catalog
            .set_metadata(FUNCTIONS_METADATA_KEY, &json)
            .map_err(|err| SQLError::Internal(format!("persist function catalog: {err}")))
    }

    pub(crate) fn restore_sql_functions_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
            return Ok(());
        };
        let defs = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json)?;
        let mut restored: BTreeMap<String, Vec<Arc<SQLUserFunction>>> = BTreeMap::new();
        for (stored_name, overloads) in defs {
            let stored_relation =
                RelationIdentity::from_legacy_name(&stored_name).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "invalid persisted routine registry key `{stored_name}`: {error}"
                    ))
                })?;
            if !self
                .durable
                .schemas
                .read()
                .contains(&stored_relation.schema)
            {
                return Err(StorageBackendError::Other(format!(
                    "persisted routine `{stored_name}` references missing schema `{}`",
                    stored_relation.schema
                )));
            }
            let canonical_name = stored_relation.qualified_name();
            for mut def in overloads {
                let definition_relation =
                    RelationIdentity::from_legacy_name(&def.name).map_err(|error| {
                        StorageBackendError::Other(format!(
                            "invalid persisted routine definition name `{}`: {error}",
                            def.name
                        ))
                    })?;
                if definition_relation != stored_relation {
                    return Err(StorageBackendError::Other(format!(
                        "persisted routine registry key `{stored_name}` does not match definition `{}`",
                        def.name
                    )));
                }
                def.name.clone_from(&canonical_name);
                let signature = routine_signature_types(&def);
                let compiled_overloads = restored.entry(canonical_name.clone()).or_default();
                if compiled_overloads
                    .iter()
                    .any(|function| routine_signature_types(&function.def) == signature)
                {
                    return Err(StorageBackendError::Other(format!(
                        "duplicate persisted routine identity `{}`",
                        routine_signature_label(&canonical_name, &signature)
                    )));
                }
                let compiled = compile_function_body(self, &def)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
                compiled_overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            }
        }
        for overloads in restored.values_mut() {
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
        }
        *self.durable.sql_user_functions.write() = restored;
        Ok(())
    }
}

/// `CREATE OR REPLACE` may not change the declared result shape.
fn same_return_shape(a: &CreateFunction, b: &CreateFunction) -> bool {
    use uqa_sql::ast::FunctionReturns;
    let same_outputs = {
        let a_outs = a.output_params();
        let b_outs = b.output_params();
        a_outs.len() == b_outs.len()
            && a_outs.iter().zip(&b_outs).all(|(x, y)| {
                x.name == y.name
                    && canonical_routine_type_name(&x.type_name)
                        == canonical_routine_type_name(&y.type_name)
                    && x.mode == y.mode
            })
    };
    let same_kind = match (&a.returns, &b.returns) {
        (FunctionReturns::None, FunctionReturns::None)
        | (FunctionReturns::Table, FunctionReturns::Table) => true,
        (FunctionReturns::Scalar { type_name: x }, FunctionReturns::Scalar { type_name: y })
        | (FunctionReturns::SetOf { type_name: x }, FunctionReturns::SetOf { type_name: y }) => {
            canonical_routine_type_name(x) == canonical_routine_type_name(y)
        }
        _ => false,
    };
    same_kind && same_outputs
}
