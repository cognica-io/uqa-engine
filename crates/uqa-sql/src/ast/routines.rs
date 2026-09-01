//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Deserialize, Expr, FunctionParallel, RoutineAclEntry, RoutineConfigAction,
    RoutineSecurityAttributes, Serialize, Statement,
};

/// Parameter mode of a `CREATE FUNCTION` / `CREATE PROCEDURE`
/// argument. Mirrors `PostgreSQL`'s `FunctionParameterMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionParamMode {
    /// `IN` (also the default when no mode is written).
    In,
    /// `OUT` - shapes the result row, not part of a function's call
    /// signature (but part of a procedure's).
    Out,
    /// `INOUT` - accepted as input and returned in the result row.
    InOut,
    /// `VARIADIC` - a trailing array parameter that accepts either expanded element arguments or one explicit `VARIADIC` array argument.
    Variadic,
    /// `RETURNS TABLE (col type, ...)` column. Behaves like an `OUT`
    /// parameter of a set-returning function.
    Table,
}

/// One declared parameter of a user-defined function or procedure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionParam {
    /// Parameter name. Empty for unnamed parameters (`f(integer)`),
    /// which are only addressable as `$n`.
    pub name: String,
    /// Raw type name as written (last segment, lower-cased by the
    /// compiler; e.g. `int4`, `text`, `numeric`).
    pub type_name: String,
    /// Parsed relation and column identity for `%TYPE`; ordinary types have no reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_reference: Option<RoutineColumnTypeReference>,
    pub mode: FunctionParamMode,
    /// `DEFAULT <expr>` for trailing input parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Expr>,
}

/// Structured relation-column identity carried by a routine `%TYPE` declaration until catalog binding resolves it to a concrete SQL type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineColumnTypeReference {
    pub schema: Option<String>,
    pub relation: String,
    pub column: String,
}

impl RoutineColumnTypeReference {
    pub fn new(schema: Option<String>, relation: String, column: String) -> Self {
        Self {
            schema,
            relation,
            column,
        }
    }

    pub fn relation_reference(&self) -> String {
        match self.schema.as_deref() {
            Some(schema) => format!(
                "{}.{}",
                render_identifier_component(schema),
                render_identifier_component(&self.relation)
            ),
            None => render_identifier_component(&self.relation),
        }
    }

    pub fn type_reference(&self) -> String {
        format!(
            "{}.{}%type",
            self.relation_reference(),
            render_identifier_component(&self.column)
        )
    }
}

fn render_identifier_component(component: &str) -> String {
    let can_render_bare = component
        .bytes()
        .enumerate()
        .all(|(index, byte)| match byte {
            b'a'..=b'z' | b'_' => true,
            b'0'..=b'9' | b'$' => index != 0,
            _ => false,
        });
    if can_render_bare && !component.is_empty() {
        component.to_string()
    } else {
        format!("\"{}\"", component.replace('"', "\"\""))
    }
}

/// Declared result shape of a user-defined function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunctionReturns {
    /// Procedures and functions whose result is shaped purely by
    /// `OUT` parameters carry no explicit `RETURNS` clause.
    None,
    /// `RETURNS <type>` - includes `RETURNS void` and `RETURNS record`.
    Scalar { type_name: String },
    /// `RETURNS SETOF <type>`.
    SetOf { type_name: String },
    /// `RETURNS TABLE (...)`. The column list lives in
    /// [`CreateFunction::params`] as [`FunctionParamMode::Table`]
    /// entries; this variant just records the set-returning shape.
    Table,
}

/// `IMMUTABLE` / `STABLE` / `VOLATILE` marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FunctionVolatility {
    Immutable,
    Stable,
    #[default]
    Volatile,
}

/// Body of a user-defined routine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunctionBody {
    /// `AS $$ ... $$` - raw source text, parsed per language at
    /// registration time.
    Source(String),
    /// SQL-standard body (`BEGIN ATOMIC ... END` / `RETURN expr`)
    /// compiled straight to statements.
    Statements(Vec<Statement>),
}

/// `CREATE [OR REPLACE] FUNCTION | PROCEDURE`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateFunction {
    pub name: String,
    pub or_replace: bool,
    pub is_procedure: bool,
    pub params: Vec<FunctionParam>,
    pub returns: FunctionReturns,
    /// Parsed `%TYPE` identity for a scalar or set return declaration until registration resolves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type_reference: Option<RoutineColumnTypeReference>,
    /// Lower-cased language name (`plpgsql`, `sql`).
    pub language: String,
    pub body: FunctionBody,
    /// Effective schema search path captured when a SQL-standard body is catalog-bound. String and PL/pgSQL bodies keep dynamic lookup and leave this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creation_search_path: Vec<String>,
    pub volatility: FunctionVolatility,
    /// `STRICT` / `RETURNS NULL ON NULL INPUT` - the function is not
    /// invoked when any input argument is NULL; the result is NULL.
    pub strict: bool,
    /// Catalog owner. The compiler leaves this empty and registration captures the effective current user; persisted definitions always carry a role name.
    #[serde(default)]
    pub owner: String,
    /// Execution identity and leakproofness, flattened to retain the catalog-definition wire shape.
    #[serde(default, flatten)]
    pub security: RoutineSecurityAttributes,
    /// Parallel-safety classification.
    #[serde(default)]
    pub parallel: FunctionParallel,
    /// Optional planner support routine identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<String>,
    /// Effective per-routine configuration as `name=value` pairs in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<(String, String)>,
    /// Creation-time configuration actions awaiting engine/session resolution. Registration consumes this list before persistence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_actions: Vec<RoutineConfigAction>,
    /// Explicit execution privileges. `None` means the `PostgreSQL` default (`PUBLIC=EXECUTE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execute_acl: Option<Vec<RoutineAclEntry>>,
}

impl CreateFunction {
    /// Parameters that define routine identity: `IN` + `INOUT` + `VARIADIC`, in declaration order.
    pub fn identity_params(&self) -> Vec<&FunctionParam> {
        self.params
            .iter()
            .filter(|param| Self::is_identity_param(param))
            .collect()
    }

    /// Number of parameters that define routine identity.
    pub fn identity_arity(&self) -> usize {
        self.params
            .iter()
            .filter(|param| Self::is_identity_param(param))
            .count()
    }

    fn is_identity_param(param: &FunctionParam) -> bool {
        matches!(
            param.mode,
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic
        )
    }

    /// Parameters supplied by a call: identity parameters for functions and every non-`TABLE` parameter for procedures.
    pub fn call_params(&self) -> Vec<&FunctionParam> {
        self.params
            .iter()
            .filter(|param| self.is_call_param(param))
            .collect()
    }

    /// Number of declared call parameters; a variadic parameter can consume multiple actual arguments.
    pub fn call_arity(&self) -> usize {
        self.params
            .iter()
            .filter(|param| self.is_call_param(param))
            .count()
    }

    /// Minimum number of actual arguments for ordinary expanded notation; a variadic parameter accepts zero elements.
    pub fn required_call_arity(&self) -> usize {
        self.params
            .iter()
            .filter(|param| {
                self.is_call_param(param)
                    && param.default.is_none()
                    && param.mode != FunctionParamMode::Variadic
            })
            .count()
    }

    fn is_call_param(&self, param: &FunctionParam) -> bool {
        match param.mode {
            FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic => true,
            FunctionParamMode::Out => self.is_procedure,
            FunctionParamMode::Table => false,
        }
    }

    /// Backward-compatible alias for [`Self::call_arity`].
    pub fn signature_arity(&self) -> usize {
        self.call_arity()
    }

    /// Backward-compatible alias for [`Self::required_call_arity`].
    pub fn required_arity(&self) -> usize {
        self.required_call_arity()
    }

    /// Backward-compatible alias for [`Self::call_params`].
    pub fn signature_params(&self) -> Vec<&FunctionParam> {
        self.call_params()
    }

    /// Parameters that shape the result row: `OUT` + `INOUT` +
    /// `RETURNS TABLE` columns, in declaration order.
    pub fn output_params(&self) -> Vec<&FunctionParam> {
        self.params
            .iter()
            .filter(|p| {
                matches!(
                    p.mode,
                    FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
                )
            })
            .collect()
    }

    /// True when the routine produces a row set (`RETURNS SETOF` /
    /// `RETURNS TABLE`).
    pub fn returns_set(&self) -> bool {
        matches!(
            self.returns,
            FunctionReturns::SetOf { .. } | FunctionReturns::Table
        )
    }
}

/// One `DROP FUNCTION` / `DROP PROCEDURE` target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropFunctionItem {
    pub name: String,
    /// `Some(types)` when the statement spelled an argument list
    /// (`DROP FUNCTION f(int, int)` - matched by canonical argument
    /// types); `None` for the bare-name form
    /// (`DROP FUNCTION f`).
    pub arg_types: Option<Vec<String>>,
}

/// `DROP FUNCTION [IF EXISTS] name[(argtypes)] [, ...]` and the
/// `DROP PROCEDURE` equivalent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropFunctionStmt {
    pub is_procedure: bool,
    pub if_exists: bool,
    #[serde(default)]
    pub cascade: bool,
    pub items: Vec<DropFunctionItem>,
}
