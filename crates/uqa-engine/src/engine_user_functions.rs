//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registry for user-defined SQL / `PL/pgSQL` routines (`CREATE
//! FUNCTION` / `CREATE PROCEDURE`). Definitions persist to catalog
//! metadata like views and sequences; the compiled body is rebuilt
//! from the definition at registration and restore time.

use std::collections::BTreeMap;

use uqa_execution::ScalarExpr;
use uqa_planner::UnifiedPlan;
use uqa_sql::ast::{CreateFunction, DropFunctionItem, DropFunctionStmt, FunctionBody, Statement};
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

/// Canonical type spelling used by routine identity and overload
/// resolution. `PostgreSQL` aliases such as `int`, `integer`, and `int4`
/// identify the same argument type even when their source spelling differs.
pub(crate) fn canonical_routine_type_name(type_name: &str) -> String {
    let compact = type_name
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(element) = compact.strip_suffix("[]") {
        return format!("{}[]", canonical_routine_type_name(element));
    }
    let without_catalog = compact.strip_prefix("pg_catalog.").unwrap_or(&compact);
    let base = without_catalog
        .split_once('(')
        .map_or(without_catalog, |(base, _)| base.trim());
    match base {
        "smallint" | "int2" => "int2",
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "real" | "float4" => "float4",
        "double" | "double precision" | "float8" => "float8",
        "decimal" | "numeric" => "numeric",
        "character varying" | "varchar" => "varchar",
        "character" | "char" | "bpchar" => "bpchar",
        "bool" | "boolean" => "bool",
        "timestamp without time zone" | "timestamp" => "timestamp",
        "timestamp with time zone" | "timestamptz" => "timestamptz",
        "time without time zone" | "time" => "time",
        "time with time zone" | "timetz" => "timetz",
        other => other,
    }
    .to_string()
}

pub(crate) fn routine_signature_types(def: &CreateFunction) -> Vec<String> {
    def.signature_params()
        .iter()
        .map(|parameter| canonical_routine_type_name(&parameter.type_name))
        .collect()
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
            Ok(CompiledFunctionBody::PLpgSQL(
                uqa_sql::plpgsql::parse_function(def)?,
            ))
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
        .map(|parameter| parameter.name.to_ascii_lowercase())
        .collect();
    statements
        .into_iter()
        .map(|statement| {
            let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
                engine.has_registered_aggregate_function(name)
            });
            plan.rewrite_scalar_expressions(&mut |expression| {
                let parameter = match expression {
                    ScalarExpr::Column(name) => parameter_names.iter().position(|parameter| {
                        !parameter.is_empty() && parameter == &name.to_ascii_lowercase()
                    }),
                    ScalarExpr::QualifiedColumn {
                        qualifier, column, ..
                    } if qualifier.eq_ignore_ascii_case(&local_name) => {
                        parameter_names.iter().position(|parameter| {
                            !parameter.is_empty() && parameter == &column.to_ascii_lowercase()
                        })
                    }
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
