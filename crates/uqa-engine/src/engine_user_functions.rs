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

use uqa_sql::ast::{CreateFunction, DropFunctionStmt, FunctionBody, Statement};
use uqa_sql::SQLError;

use super::{Arc, CatalogFacade, Engine, StorageBackendResult, FUNCTIONS_METADATA_KEY};

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
    SQL(Vec<Statement>),
}

/// Compile a routine body per its language. Shared by DDL
/// registration and restore-from-catalog.
pub(crate) fn compile_function_body(
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
        "sql" => match &def.body {
            FunctionBody::Source(source) => {
                Ok(CompiledFunctionBody::SQL(uqa_sql::compile(source)?))
            }
            FunctionBody::Statements(stmts) => Ok(CompiledFunctionBody::SQL(stmts.clone())),
        },
        other => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{other}\" does not exist"),
        }),
    }
}

impl Engine {
    /// Register (or replace) a user-defined routine. Applies the
    /// `PostgreSQL` conflict rules for `(name, signature arity)`
    /// collisions and persists the updated overload set.
    pub(crate) fn register_sql_function(&self, def: CreateFunction) -> Result<(), SQLError> {
        let compiled = compile_function_body(&def)?;
        let name = def.name.clone();
        {
            let mut registry = self.sql_user_functions.write();
            let overloads = registry.entry(name).or_default();
            let arity = def.signature_arity();
            if let Some(pos) = overloads
                .iter()
                .position(|f| f.def.signature_arity() == arity)
            {
                let existing = &overloads[pos].def;
                if !def.or_replace {
                    return Err(SQLError::Routine {
                        sqlstate: "42723".into(),
                        message: format!(
                            "function \"{}\" already exists with same argument types",
                            def.name
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
        }
        self.persist_sql_functions();
        Ok(())
    }

    /// Drop routines per a `DROP FUNCTION` / `DROP PROCEDURE`
    /// statement, mirroring `PostgreSQL`'s resolution and error
    /// texts. `IF EXISTS` misses emit a notice instead of failing.
    pub(crate) fn drop_sql_functions(&self, stmt: &DropFunctionStmt) -> Result<(), SQLError> {
        let kind = if stmt.is_procedure {
            "procedure"
        } else {
            "function"
        };
        let mut dropped = false;
        for item in &stmt.items {
            let mut registry = self.sql_user_functions.write();
            let overloads = registry.get_mut(&item.name);
            let position = match (&overloads, &item.arg_types) {
                (Some(list), Some(types)) => list
                    .iter()
                    .position(|f| f.def.signature_arity() == types.len()),
                (Some(list), None) => {
                    if list.len() > 1 {
                        return Err(SQLError::Routine {
                            sqlstate: "42725".into(),
                            message: format!("{kind} name \"{}\" is not unique", item.name),
                        });
                    }
                    if list.is_empty() {
                        None
                    } else {
                        Some(0)
                    }
                }
                (None, _) => None,
            };
            if let (Some(list), Some(pos)) = (overloads, position) {
                list.remove(pos);
                if list.is_empty() {
                    registry.remove(&item.name);
                }
                dropped = true;
            } else {
                drop(registry);
                let spelled = match &item.arg_types {
                    Some(types) => format!("{}({})", item.name, types.join(", ")),
                    None => format!("{}()", item.name),
                };
                if stmt.if_exists {
                    self.push_sql_notice(
                        "NOTICE",
                        &format!("{kind} {spelled} does not exist, skipping"),
                    );
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
            self.persist_sql_functions();
        }
        Ok(())
    }

    /// Overload set registered under `name` (lower-cased lookup).
    pub(crate) fn lookup_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.sql_user_functions
            .read()
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    /// Every registered routine, sorted by name then arity. Feeds
    /// `pg_catalog.pg_proc` / `information_schema.routines`.
    pub(crate) fn list_sql_functions(&self) -> Vec<Arc<SQLUserFunction>> {
        let registry = self.sql_user_functions.read();
        let mut out: Vec<Arc<SQLUserFunction>> = Vec::new();
        for overloads in registry.values() {
            let mut sorted = overloads.clone();
            sorted.sort_by_key(|f| f.def.signature_arity());
            out.extend(sorted);
        }
        out
    }

    /// Current nesting cap for user-defined routine calls.
    pub fn sql_function_depth_limit(&self) -> usize {
        self.sql_function_depth_limit
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adjust the nesting cap for user-defined routine calls
    /// (minimum 1). Mirrors `PostgreSQL`'s `max_stack_depth` role for
    /// recursive functions.
    pub fn set_sql_function_depth_limit(&self, limit: usize) {
        self.sql_function_depth_limit
            .store(limit.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Queue a notice (`RAISE NOTICE` / `WARNING` / ...).
    pub(crate) fn push_sql_notice(&self, level: &str, message: &str) {
        self.sql_notices
            .lock()
            .push((level.to_string(), message.to_string()));
    }

    /// Drain queued notices as `(level, message)` pairs in emission
    /// order.
    pub fn take_sql_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.sql_notices.lock())
    }

    fn persist_sql_functions(&self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let defs: BTreeMap<String, Vec<CreateFunction>> = self
            .sql_user_functions
            .read()
            .iter()
            .map(|(name, overloads)| {
                (
                    name.clone(),
                    overloads.iter().map(|f| f.def.clone()).collect(),
                )
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&defs) {
            let _ = catalog.set_metadata(FUNCTIONS_METADATA_KEY, &json);
        }
    }

    pub(crate) fn restore_sql_functions_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
            return Ok(());
        };
        let Ok(defs) = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json) else {
            return Ok(());
        };
        let mut registry = self.sql_user_functions.write();
        for (name, overloads) in defs {
            let mut compiled_overloads = Vec::with_capacity(overloads.len());
            for def in overloads {
                // A definition that no longer compiles (e.g. written
                // by a newer engine) is skipped rather than blocking
                // the whole catalog restore.
                if let Ok(compiled) = compile_function_body(&def) {
                    compiled_overloads.push(Arc::new(SQLUserFunction { def, compiled }));
                }
            }
            if !compiled_overloads.is_empty() {
                registry.insert(name, compiled_overloads);
            }
        }
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
            && a_outs
                .iter()
                .zip(&b_outs)
                .all(|(x, y)| x.name == y.name && x.type_name == y.type_name && x.mode == y.mode)
    };
    let same_kind = match (&a.returns, &b.returns) {
        (FunctionReturns::None, FunctionReturns::None)
        | (FunctionReturns::Table, FunctionReturns::Table) => true,
        (FunctionReturns::Scalar { type_name: x }, FunctionReturns::Scalar { type_name: y })
        | (FunctionReturns::SetOf { type_name: x }, FunctionReturns::SetOf { type_name: y }) => {
            x == y
        }
        _ => false,
    };
    same_kind && same_outputs
}
