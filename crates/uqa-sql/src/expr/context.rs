//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation context, row lookup, and engine-backed type resolution.

use std::borrow::Cow;

use uqa_core::Value;

use crate::ast::{ColumnType, InternalColumnRef};
use crate::error::{Result, SQLError};
use crate::params::SQLParam;
use crate::result::ResultRow;

use super::casting::cast_value_from;
use super::conversion::{array_value_to_string, value_to_string};

#[must_use]
pub fn coercion_type_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::Domain { base, .. } => coercion_type_name(base),
        ColumnType::Array(element) => format!("{}[]", coercion_type_name(element)),
        _ => ty.sql_name(),
    }
}

/// Engine-side hook that scalar function evaluation calls for stateful
/// sequence and user-defined functions. Query-valued expressions are not
/// accepted here: lowering assigns them physical query-plan slots executed by
/// `uqa-execution::ScalarSubqueryRunner`.
pub trait EngineHook {
    fn nextval(&self, name: &str) -> Result<i64>;
    fn currval(&self, name: &str) -> Result<i64>;
    fn setval(&self, name: &str, value: i64) -> Result<i64>;

    fn call_scalar_function(&self, _name: &str, _args: &[Value]) -> Option<Result<Value>> {
        None
    }

    /// Invoke an engine-backed built-in after an exact catalog binding has
    /// selected it. Unlike `call_scalar_function`, this path is also available
    /// when dynamic dispatch is disabled, so runtime callbacks cannot override
    /// the stored built-in identity.
    fn call_bound_builtin_function(
        &self,
        _binding: &crate::ast::FunctionBinding,
        _args: &[(Option<String>, Value)],
    ) -> Option<Result<Value>> {
        None
    }

    fn has_scalar_functions(&self) -> bool {
        true
    }

    /// Resolve a catalog-owned SQL type name for casts evaluated with an engine context.
    fn resolve_type_name(&self, _name: &str) -> std::result::Result<Option<ColumnType>, String> {
        Ok(None)
    }

    /// Resolve a relation name to the OID carrier used by `regclass`.
    fn resolve_regclass(&self, _name: &str) -> std::result::Result<Option<i64>, String> {
        Ok(None)
    }

    /// Resolve an exact routine signature to the OID carrier used by `regprocedure`.
    fn resolve_regprocedure(&self, _name: &str) -> std::result::Result<Option<i64>, String> {
        Ok(None)
    }

    /// Resolve one OID-backed alias type to its `PostgreSQL` text output.
    fn resolve_regtype_output(
        &self,
        _ty: &ColumnType,
        _oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }

    /// Resolve the first existing schema on the logical session's search
    /// path. `None` lets standalone expression evaluation use its `public`
    /// compatibility default.
    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }

    fn current_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }

    fn session_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(None)
    }

    /// Resolve the existing schemas visible to the logical session.
    fn current_schemas(
        &self,
        _include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        Ok(None)
    }

    /// Draw from an engine-owned logical-session PRNG. `None` keeps pure,
    /// engine-free expression evaluation available for library callers.
    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(None)
    }

    /// Draw every bit of one engine-owned logical-session PRNG word. Range
    /// functions use this instead of a floating-point sample so `bigint` and
    /// arbitrary-precision `numeric` bounds remain uniform.
    fn random_u64(&self) -> std::result::Result<Option<u64>, String> {
        Ok(None)
    }

    /// Reseed the logical-session PRNG. `false` means the hook does not own a
    /// mutable random stream and the caller must report the unsupported call.
    fn set_random_seed(&self, _seed: f64) -> std::result::Result<bool, String> {
        Ok(false)
    }

    /// Invoke a user-defined SQL / `PL/pgSQL` function. Consulted
    /// after built-in dispatch misses (and immediately for calls with
    /// named arguments, which built-ins never accept). `None` means
    /// no user-defined function with this name exists.
    fn call_user_function(
        &self,
        _name: &str,
        _args: &[(Option<String>, Value)],
    ) -> Option<Result<Value>> {
        None
    }

    fn call_bound_user_function(
        &self,
        _binding: &crate::ast::FunctionBinding,
        _args: &[(Option<String>, Value)],
    ) -> Option<Result<Value>> {
        None
    }
}

/// Format a scalar or array OID carrier using the catalog-aware output function of a `reg*` type. `None` means the declared type is not one of the supported alias types or the value is SQL NULL.
pub fn format_regtype_value(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<Option<String>> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    if let ColumnType::Array(element) = ty {
        if !matches!(
            element.as_ref(),
            ColumnType::Regproc
                | ColumnType::Regprocedure
                | ColumnType::Regclass
                | ColumnType::Regnamespace
                | ColumnType::Regtype
        ) {
            return Ok(None);
        }
        let Value::Array(array) = value else {
            return Ok(Some(value_to_string(value)));
        };
        let elements = format_regtype_array_elements(array.elements(), element, engine)?;
        let formatted = array.with_elements(elements).ok_or_else(|| {
            SQLError::Internal("regtype array output changed the array dimensions".into())
        })?;
        return Ok(Some(array_value_to_string(&formatted)));
    }
    if !matches!(
        ty,
        ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regtype
    ) {
        return Ok(None);
    }
    let Value::Int(oid) = value else {
        return Ok(Some(value_to_string(value)));
    };
    if *oid == 0 {
        return Ok(Some("-".into()));
    }
    let resolved = engine
        .map(|engine| engine.resolve_regtype_output(ty, *oid))
        .transpose()
        .map_err(SQLError::Internal)?
        .flatten();
    Ok(Some(resolved.unwrap_or_else(|| oid.to_string())))
}

fn format_regtype_array_elements(
    values: &[Value],
    element: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<Vec<Value>> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => Ok(Value::Null),
            Value::List(nested) => {
                format_regtype_array_elements(nested, element, engine).map(Value::List)
            }
            other => format_regtype_value(other, element, engine)
                .map(|text| text.map_or_else(|| other.clone(), Value::Str)),
        })
        .collect()
}

/// Cast a value after resolving catalog-owned source and target types and flattening domains to their coercion types.
pub fn cast_value_with_type_resolution(
    value: &Value,
    source_ty: Option<&str>,
    target_ty: &str,
    engine: Option<&dyn EngineHook>,
) -> Result<Value> {
    let resolved_source = match (engine, source_ty) {
        (Some(engine), Some(source_ty)) => engine
            .resolve_type_name(source_ty)
            .map_err(SQLError::Internal)?
            .map(|ty| coercion_type_name(&ty)),
        _ => None,
    };
    let source_ty = resolved_source.as_deref().or(source_ty);
    let resolved_target = engine
        .map(|engine| engine.resolve_type_name(target_ty))
        .transpose()
        .map_err(SQLError::Internal)?
        .flatten();
    let target_ty = resolved_target.as_ref().map_or_else(
        || Cow::Borrowed(target_ty),
        |ty| Cow::Owned(coercion_type_name(ty)),
    );
    if target_ty.eq_ignore_ascii_case("text") {
        if let Some(source_ty) = source_ty.and_then(|source| ColumnType::from_sql_name(source).ok())
        {
            if let Some(text) = format_regtype_value(value, &source_ty, engine)? {
                return Ok(Value::Str(text));
            }
        }
    }
    if target_ty.eq_ignore_ascii_case("regclass") {
        if let (Some(engine), Value::Str(name) | Value::FixedChar(name)) = (engine, value) {
            return engine
                .resolve_regclass(name)
                .map_err(SQLError::Internal)?
                .map(Value::Int)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{name}\" does not exist"),
                });
        }
    }
    if target_ty.eq_ignore_ascii_case("regprocedure") {
        if let (Some(engine), Value::Str(name) | Value::FixedChar(name)) = (engine, value) {
            return engine
                .resolve_regprocedure(name)
                .map_err(SQLError::Internal)?
                .map(Value::Int)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: format!("function {name} does not exist"),
                });
        }
    }
    cast_value_from(value, &target_ty, source_ty)
}

/// Read-only row interface used by the expression evaluator. Most callers
/// use a materialised [`ResultRow`], while hot execution paths can expose a
/// projected value slice without rebuilding a string-keyed map for every row.
pub trait RowLookup {
    fn column(&self, name: &str) -> Option<&Value>;

    /// Whether an unqualified name identifies more than one visible input
    /// column. Callers must report SQLSTATE 42702 instead of selecting an
    /// arbitrary suffix match.
    fn column_is_ambiguous(&self, _name: &str) -> bool {
        false
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value>;

    /// Whether a qualified identity names more than one visible input column.
    fn qualified_column_is_ambiguous(&self, _qualifier: &str, _column: &str) -> bool {
        false
    }

    /// Return a value by the physical schema position used to construct this
    /// row view. Materialized named rows do not expose positional access;
    /// projected execution sources override it so compiled hot paths can avoid
    /// repeating string lookup for every expression and row.
    fn positional_column(&self, _index: usize) -> Option<&Value> {
        None
    }

    /// Resolve an executor-only relation attribute. Materialized SQL rows do
    /// not expose these structural slots.
    fn internal_column(&self, _column: InternalColumnRef) -> Option<&Value> {
        None
    }

    /// Read the structurally carried retrieval score for one relation. The qualifier selects a score-bearing source without exposing an executor field in the SQL column namespace.
    fn score_source(&self, _qualifier: Option<&str>) -> Option<&Value> {
        None
    }

    /// Whether the requested score source resolves to more than one retrieval relation.
    fn score_source_is_ambiguous(&self, _qualifier: Option<&str>) -> bool {
        false
    }

    /// Visit every logical column in schema order. Named rows use their map
    /// order; positional execution rows override this without materializing a
    /// map. The default keeps narrow projected lookup implementations source
    /// compatible when they deliberately do not expose whole-row semantics.
    fn visit_columns(&self, _visitor: &mut dyn FnMut(&str, &Value)) {}
}

impl RowLookup for ResultRow {
    fn column(&self, name: &str) -> Option<&Value> {
        self.get(name)
    }

    fn qualified_column(&self, _qualifier: &str, _column: &str) -> Option<&Value> {
        None
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (column, value) in self {
            visitor(column, value);
        }
    }
}

pub struct EvalContext<'a> {
    pub row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    pub params: &'a [SQLParam],
    pub engine: Option<&'a dyn EngineHook>,
}

impl<'a> EvalContext<'a> {
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            params,
            engine: None,
        }
    }

    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            // Whole-row materialization is needed only by correlated
            // subqueries. Ordinary scalar evaluation must remain on the
            // lookup/slot path.
            row: None,
            row_lookup: Some(row),
            params,
            engine: None,
        }
    }

    pub fn with_engine(mut self, engine: &'a dyn EngineHook) -> Self {
        self.engine = Some(engine);
        self
    }

    pub(super) fn row_lookup(&self) -> Result<&'a dyn RowLookup> {
        self.row_lookup
            .ok_or_else(|| SQLError::Internal("column reference without row context".into()))
    }

    /// Resolve an unqualified column through the same row semantics used by
    /// the AST evaluator. Physical scalar IR evaluators call this instead of
    /// reconstructing an [`Expr::Column`](crate::ast::Expr::Column) carrier.
    pub fn column_value(&self, name: &str) -> Result<Value> {
        if self.row_lookup()?.column_is_ambiguous(name) {
            return Err(SQLError::AmbiguousColumn(name.to_string()));
        }
        Ok(self
            .row_lookup()?
            .column(name)
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Resolve a qualified column without constructing an AST expression.
    pub fn qualified_column_value(&self, qualifier: &str, column: &str) -> Result<Value> {
        if self
            .row_lookup()?
            .qualified_column_is_ambiguous(qualifier, column)
        {
            return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
        }
        Ok(self
            .row_lookup()?
            .qualified_column(qualifier, column)
            .cloned()
            .unwrap_or(Value::Null))
    }
}
