//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal SQL AST. Lifts the relevant subset of the `libpg_query`
//! protobuf tree into a Rust enum the compiler walks. Statements not
//! yet supported parse cleanly but compile to
//! [`crate::SQLError::Unsupported`].

use serde::{Deserialize, Serialize};

mod expressions;
mod locking;

pub use expressions::*;
pub use locking::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnType {
    SmallInteger,
    Integer,
    BigInteger,
    /// `PostgreSQL` object identifier (`pg_catalog.oid`).
    Oid,
    /// `PostgreSQL` transaction identifier (`pg_catalog.xid`).
    Xid,
    Boolean,
    Text,
    Name,
    Uuid,
    Varchar(Option<u32>),
    /// Internal unconstrained `bpchar` type used after common-type selection.
    Bpchar,
    /// `PostgreSQL` blank-padded `CHARACTER(n)` / `CHAR(n)` (`bpchar`).
    /// The length counts Unicode scalar values and defaults to one when the
    /// declaration omits an explicit modifier.
    Character(u32),
    Real,
    DoublePrecision,
    /// `NUMERIC(precision, scale)` -- exact decimal storage. When
    /// `scale` is `Some(s)` the engine rounds `INSERT` values to `s`
    /// fractional digits. `precision` is captured for round-tripping
    /// the catalog text but is not currently enforced.
    Numeric {
        precision: Option<u32>,
        scale: Option<i32>,
    },
    /// `JSON` / `JSONB` columns store typed JSON values.
    Json,
    /// `JSONB` columns store typed JSON values with `PostgreSQL` JSONB operators.
    JsonB,
    /// `BYTEA` columns store opaque bytes.
    Bytea,
    /// `PostgreSQL`'s internal single-byte `"char"` catalog type.
    InternalChar,
    Regproc,
    /// `PostgreSQL` relation object identifier (`pg_catalog.regclass`).
    Regclass,
    /// `PostgreSQL` namespace object identifier (`pg_catalog.regnamespace`).
    Regnamespace,
    Regtype,
    PgNodeTree,
    AclItem,
    Int2Vector,
    OidVector,
    AnyArray,
    /// `PostgreSQL`'s anonymous composite pseudo-type (OID 2249).
    Record,
    /// A `PostgreSQL` array whose elements retain their declared SQL type.
    /// Nested array bounds are represented recursively.
    Array(Box<ColumnType>),
    /// `DATE` columns store days since 1970-01-01.
    Date,
    /// `TIME` columns store microseconds since midnight.
    Time,
    /// `TIME WITH TIME ZONE` columns store local time plus offset.
    TimeTz,
    /// `TIMESTAMP WITHOUT TIME ZONE` columns store naive microseconds
    /// since 1970-01-01 00:00:00.
    Timestamp,
    /// `TIMESTAMP WITH TIME ZONE` columns store UTC microseconds since
    /// 1970-01-01 00:00:00Z.
    TimestampTz,
    Interval,
    /// `VECTOR(N)` columns store an `N`-dimensional `f32` embedding.
    Vector(u32),
    /// `TENSOR(N)` columns store an array of `N`-dimensional `f32`
    /// embeddings. The row remains the retrieval identity; vector
    /// indexes score against the best element in the tensor.
    Tensor(u32),
    /// A named `PostgreSQL` domain retaining both its own type identity and the
    /// base type used for value conversion and operator selection.
    Domain {
        schema: String,
        name: String,
        oid: u32,
        base: Box<ColumnType>,
    },
}

pub(crate) fn builtin_array_element_name(type_name: &str) -> Option<&'static str> {
    Some(match type_name {
        "_bool" => "bool",
        "_bytea" => "bytea",
        "_char" => "\"char\"",
        "_name" => "name",
        "_int8" => "int8",
        "_int2" => "int2",
        "_int2vector" => "int2vector",
        "_int4" => "int4",
        "_regproc" => "regproc",
        "_regclass" => "regclass",
        "_text" => "text",
        "_oid" => "oid",
        "_oidvector" => "oidvector",
        "_bpchar" => "bpchar",
        "_varchar" => "varchar",
        "_float4" => "float4",
        "_float8" => "float8",
        "_aclitem" => "aclitem",
        "_date" => "date",
        "_time" => "time",
        "_timestamp" => "timestamp",
        "_timestamptz" => "timestamptz",
        "_interval" => "interval",
        "_numeric" => "numeric",
        "_timetz" => "timetz",
        "_record" => "record",
        "_uuid" => "uuid",
        "_json" => "json",
        "_jsonb" => "jsonb",
        "_regtype" => "regtype",
        "_xid" => "xid",
        "_pg_node_tree" => "pg_node_tree",
        _ => return None,
    })
}

impl ColumnType {
    #[must_use]
    pub fn is_integer(&self) -> bool {
        match self {
            Self::SmallInteger | Self::Integer | Self::BigInteger | Self::Oid | Self::Xid => true,
            Self::Domain { base, .. } => base.is_integer(),
            _ => false,
        }
    }

    #[must_use]
    pub fn is_character_string(&self) -> bool {
        match self {
            Self::Text
            | Self::Name
            | Self::Varchar(_)
            | Self::Bpchar
            | Self::Character(_)
            | Self::InternalChar
            | Self::PgNodeTree
            | Self::AclItem => true,
            Self::Domain { base, .. } => base.is_character_string(),
            _ => false,
        }
    }

    /// Parse the canonical or accepted spelling of one implemented SQL type.
    /// This is shared by expression binding and row-schema propagation so a
    /// cast's declared type is not reconstructed from its runtime value.
    pub fn from_sql_name(name: &str) -> Result<Self, crate::SQLError> {
        let normalized = name.trim().to_ascii_lowercase();
        if let Some(element) = builtin_array_element_name(&normalized) {
            return Self::from_sql_name(element).map(|ty| Self::Array(Box::new(ty)));
        }
        if let Some(element) = normalized.strip_suffix("[]") {
            return Self::from_sql_name(element).map(|ty| Self::Array(Box::new(ty)));
        }
        let (base, modifier) = normalized
            .strip_suffix(')')
            .and_then(|prefix| prefix.rsplit_once('('))
            .map_or((normalized.as_str(), None), |(base, modifier)| {
                (base.trim(), Some(modifier.trim()))
            });
        let base = base.strip_prefix("pg_catalog.").unwrap_or(base);
        let character_length = || -> Result<Option<u32>, crate::SQLError> {
            modifier
                .map(|value| {
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|length| *length > 0)
                        .ok_or_else(|| {
                            crate::SQLError::TypeMismatch(format!(
                                "character length must be greater than zero, got {value}"
                            ))
                        })
                })
                .transpose()
        };
        match base {
            "smallint" | "int2" | "smallserial" | "serial2" => Ok(Self::SmallInteger),
            "integer" | "int" | "int4" | "serial" | "serial4" => Ok(Self::Integer),
            "bigint" | "int8" | "bigserial" | "serial8" => Ok(Self::BigInteger),
            "oid" => Ok(Self::Oid),
            "xid" => Ok(Self::Xid),
            "boolean" | "bool" => Ok(Self::Boolean),
            "text" => Ok(Self::Text),
            "name" => Ok(Self::Name),
            "uuid" => Ok(Self::Uuid),
            "varchar" | "character varying" => Ok(Self::Varchar(character_length()?)),
            "character" | "char" => Ok(Self::Character(character_length()?.unwrap_or(1))),
            "bpchar" => Ok(character_length()?.map_or(Self::Bpchar, Self::Character)),
            "real" | "float4" => Ok(Self::Real),
            "double" | "double precision" | "float8" => Ok(Self::DoublePrecision),
            "numeric" | "decimal" => {
                let (precision, scale) = match modifier {
                    None => (None, None),
                    Some(modifier) => {
                        let mut parts = modifier.split(',').map(str::trim);
                        let precision = parts
                            .next()
                            .and_then(|value| value.parse::<u32>().ok())
                            .ok_or_else(|| {
                                crate::SQLError::TypeMismatch(format!(
                                    "invalid numeric modifier `{modifier}`"
                                ))
                            })?;
                        let scale = parts
                            .next()
                            .map(|value| value.parse::<i32>())
                            .transpose()
                            .map_err(|_| {
                                crate::SQLError::TypeMismatch(format!(
                                    "invalid numeric modifier `{modifier}`"
                                ))
                            })?
                            .unwrap_or(0);
                        if parts.next().is_some() {
                            return Err(crate::SQLError::TypeMismatch(format!(
                                "invalid numeric modifier `{modifier}`"
                            )));
                        }
                        (Some(precision), Some(scale))
                    }
                };
                Ok(Self::Numeric { precision, scale })
            }
            "json" => Ok(Self::Json),
            "jsonb" => Ok(Self::JsonB),
            "bytea" => Ok(Self::Bytea),
            "\"char\"" => Ok(Self::InternalChar),
            "regproc" => Ok(Self::Regproc),
            "regclass" => Ok(Self::Regclass),
            "regnamespace" => Ok(Self::Regnamespace),
            "regtype" => Ok(Self::Regtype),
            "pg_node_tree" => Ok(Self::PgNodeTree),
            "aclitem" => Ok(Self::AclItem),
            "int2vector" => Ok(Self::Int2Vector),
            "oidvector" => Ok(Self::OidVector),
            "anyarray" => Ok(Self::AnyArray),
            "record" => Ok(Self::Record),
            "date" => Ok(Self::Date),
            "time" | "time without time zone" => Ok(Self::Time),
            "timetz" | "time with time zone" => Ok(Self::TimeTz),
            "timestamp" | "datetime" | "timestamp without time zone" => Ok(Self::Timestamp),
            "timestamptz" | "timestamp with time zone" => Ok(Self::TimestampTz),
            "interval" => Ok(Self::Interval),
            "vector" => modifier
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|dimension| *dimension > 0)
                .map(Self::Vector)
                .ok_or_else(|| crate::SQLError::TypeMismatch("VECTOR requires a dimension".into())),
            "tensor" => modifier
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|dimension| *dimension > 0)
                .map(Self::Tensor)
                .ok_or_else(|| crate::SQLError::TypeMismatch("TENSOR requires a dimension".into())),
            other => Err(crate::SQLError::Unsupported(format!(
                "SQL type `{other}` is not supported"
            ))),
        }
    }

    #[must_use]
    pub fn sql_name(&self) -> String {
        match self {
            Self::SmallInteger => "smallint".into(),
            Self::Integer => "integer".into(),
            Self::BigInteger => "bigint".into(),
            Self::Oid => "oid".into(),
            Self::Xid => "xid".into(),
            Self::Boolean => "boolean".into(),
            Self::Text => "text".into(),
            Self::Name => "name".into(),
            Self::Uuid => "uuid".into(),
            Self::Varchar(Some(length)) => format!("character varying({length})"),
            Self::Varchar(None) => "character varying".into(),
            Self::Bpchar => "bpchar".into(),
            Self::Character(length) => format!("character({length})"),
            Self::Real => "real".into(),
            Self::DoublePrecision => "double precision".into(),
            Self::Numeric {
                precision: Some(precision),
                scale: Some(scale),
            } => format!("numeric({precision},{scale})"),
            Self::Numeric { .. } => "numeric".into(),
            Self::Json => "json".into(),
            Self::JsonB => "jsonb".into(),
            Self::Bytea => "bytea".into(),
            Self::InternalChar => "\"char\"".into(),
            Self::Regproc => "regproc".into(),
            Self::Regclass => "regclass".into(),
            Self::Regnamespace => "regnamespace".into(),
            Self::Regtype => "regtype".into(),
            Self::PgNodeTree => "pg_node_tree".into(),
            Self::AclItem => "aclitem".into(),
            Self::Int2Vector => "int2vector".into(),
            Self::OidVector => "oidvector".into(),
            Self::AnyArray => "anyarray".into(),
            Self::Record => "record".into(),
            Self::Array(element) => format!("{}[]", element.sql_name()),
            Self::Date => "date".into(),
            Self::Time => "time without time zone".into(),
            Self::TimeTz => "time with time zone".into(),
            Self::Timestamp => "timestamp without time zone".into(),
            Self::TimestampTz => "timestamp with time zone".into(),
            Self::Interval => "interval".into(),
            Self::Vector(dimension) => format!("vector({dimension})"),
            Self::Tensor(dimension) => format!("tensor({dimension})"),
            Self::Domain { schema, name, .. } => format!("{schema}.{name}"),
        }
    }

    /// Name emitted by `PostgreSQL`'s `regtype` output, including
    /// `pg_typeof(...)`.
    #[must_use]
    pub fn regtype_name(&self) -> String {
        match self {
            Self::Varchar(_) => "character varying".into(),
            Self::Bpchar | Self::Character(_) => "character".into(),
            Self::Numeric { .. } => "numeric".into(),
            Self::Vector(_) => "vector".into(),
            Self::Tensor(_) => "tensor".into(),
            Self::Domain { schema, name, .. } => format!("{schema}.{name}"),
            Self::Array(element) => format!("{}[]", element.regtype_name()),
            other => other.sql_name(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratedColumnKind {
    Virtual,
    Stored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedColumn {
    pub kind: GeneratedColumnKind,
    pub expression: Box<Expr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_dependencies: Vec<GeneratedFunctionDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionBinding {
    pub name: String,
    pub argument_types: Vec<String>,
    #[serde(default)]
    pub builtin: bool,
}

impl FunctionBinding {
    /// Construct the identity marker used when `PostgreSQL` parses a polymorphic syntax expression instead of an ordinary function call.
    #[must_use]
    pub fn polymorphic_builtin_syntax(name: &str) -> Self {
        assert!(Self::is_polymorphic_builtin_syntax_name(name));
        Self {
            name: name.into(),
            argument_types: Vec::new(),
            builtin: true,
        }
    }

    /// Return whether this binding marks a polymorphic syntax expression whose argument types must be inferred from its operands.
    #[must_use]
    pub fn is_polymorphic_builtin_syntax(&self) -> bool {
        self.builtin
            && self.argument_types.is_empty()
            && Self::is_polymorphic_builtin_syntax_name(&self.name)
    }

    /// Return whether an unqualified local name belongs to `PostgreSQL`'s polymorphic function-like syntax expressions.
    #[must_use]
    pub fn is_polymorphic_builtin_syntax_name(name: &str) -> bool {
        matches!(name, "coalesce" | "greatest" | "least" | "nullif")
    }
}

pub type GeneratedFunctionDependency = FunctionBinding;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub primary_key: bool,
    pub not_null: bool,
    /// Whether `NOT NULL` was declared as its own constraint instead of being
    /// implied by `PRIMARY KEY` or an auto-incrementing identity.
    #[serde(default)]
    pub not_null_explicit: bool,
    /// Durable `PostgreSQL` 18 `NOT NULL` constraint name. Parsing leaves an
    /// unnamed declaration as `None`; table registration assigns and persists
    /// `PostgreSQL`'s generated name before the constraint becomes visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_null_name: Option<String>,
    /// `SERIAL` / `BIGSERIAL` columns auto-allocate from a per-table
    /// monotonic counter when the value is omitted from `INSERT`.
    #[serde(default)]
    pub auto_increment: bool,
    /// `UNIQUE` column constraint -- the engine rejects an INSERT
    /// whose value for this column already exists in another row.
    #[serde(default)]
    pub unique: bool,
    /// `DEFAULT <expr>`. Evaluated at INSERT time when the column is
    /// not present in the row tuple. Persisted in catalog metadata so
    /// reopened engines keep the same INSERT semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Expr>,
    /// `PostgreSQL` 18 generated-column definition. Stored values are refreshed
    /// on every row write; virtual values are evaluated from the physical row
    /// only when a logical row is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<GeneratedColumn>,
    /// `CHECK (<expr>)` column-level constraint. Evaluated at INSERT
    /// (and UPDATE-replace) time against the row being written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Expr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
    #[serde(default = "default_true")]
    pub check_enforced: bool,
    /// `REFERENCES parent(col)` column-level FOREIGN KEY. The engine
    /// rejects INSERT / UPDATE whose value is not present in the
    /// referenced (table, column) pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ForeignKeyRef>,
}

/// `REFERENCES table(column)` reference target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub table: String,
    pub column: String,
    #[serde(default)]
    pub on_update: ForeignKeyAction,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    #[serde(default)]
    pub match_type: ForeignKeyMatch,
    #[serde(default = "default_true")]
    pub enforced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTable {
    pub name: String,
    /// Local SQL relation identifier used while binding expressions declared inside the table definition.
    pub qualifier: String,
    pub columns: Vec<ColumnDef>,
    /// `CREATE TABLE IF NOT EXISTS` - silently ignore the statement
    /// when a table with this name already exists.
    pub if_not_exists: bool,
    /// Table-level `CHECK (...)` constraints. Each entry is an
    /// expression that must evaluate truthy against every row.
    #[allow(dead_code)]
    pub checks: Vec<TableCheck>,
    /// Table-level `FOREIGN KEY (col, ...) REFERENCES parent(col, ...)`.
    pub foreign_keys: Vec<ForeignKey>,
    /// Every declared `PRIMARY KEY` / `UNIQUE` constraint, including
    /// column-level declarations. Keeping the typed key (rather than only
    /// setting per-column flags) preserves composite-key and `NULLS NOT
    /// DISTINCT` semantics through planning and catalog persistence.
    #[serde(default)]
    pub key_constraints: Vec<TableKeyConstraint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKeyConstraintKind {
    PrimaryKey,
    Unique,
}

/// A table key whose columns are compared as one tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableKeyConstraint {
    pub name: Option<String>,
    pub kind: TableKeyConstraintKind,
    pub columns: Vec<String>,
    /// `PostgreSQL` UNIQUE keys normally treat every NULL-containing tuple as
    /// distinct. `UNIQUE NULLS NOT DISTINCT` opts into NULL equality.
    #[serde(default)]
    pub nulls_not_distinct: bool,
}

/// Durable table-level constraints that do not fit in `ColumnDef`.
///
/// `serde(default)` on the catalog field containing this structure keeps
/// databases written before constraint persistence backward compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableConstraintSet {
    #[serde(default)]
    pub checks: Vec<TableCheck>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub key_constraints: Vec<TableKeyConstraint>,
}

/// `CHECK (expr)` constraint with an optional name (`CONSTRAINT <name>
/// CHECK (...)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCheck {
    pub name: Option<String>,
    pub expr: Expr,
    #[serde(default = "default_true")]
    pub enforced: bool,
}

/// Table-level foreign key. `local_columns.len()` matches
/// `ref_columns.len()`; the engine joins on the position-aligned
/// pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub local_columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    #[serde(default)]
    pub on_update: ForeignKeyAction,
    #[serde(default)]
    pub on_delete: ForeignKeyAction,
    /// Optional column subset for `ON DELETE SET NULL (...)` and
    /// `ON DELETE SET DEFAULT (...)`. Empty means every local FK
    /// column participates.
    #[serde(default)]
    pub on_delete_set_columns: Vec<String>,
    #[serde(default)]
    pub match_type: ForeignKeyMatch,
    #[serde(default = "default_true")]
    pub enforced: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ForeignKeyAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ForeignKeyMatch {
    #[default]
    Simple,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateIndex {
    pub name: Option<String>,
    pub table: String,
    /// `gin`, `btree`, `ivf`, `hnsw`, `rtree`, ...
    pub access_method: String,
    pub columns: Vec<String>,
    /// `CREATE INDEX IF NOT EXISTS`.
    pub if_not_exists: bool,
    /// Storage parameters from `WITH (k = v, ...)`. Stored verbatim;
    /// known keys (`analyzer`, `lists`, `probes`, ...)
    /// are interpreted by the engine.
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropStmt {
    pub kind: DropKind,
    pub names: Vec<String>,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropKind {
    Table,
    Index,
    View,
    Schema,
}

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
    pub volatility: FunctionVolatility,
    /// `STRICT` / `RETURNS NULL ON NULL INPUT` - the function is not
    /// invoked when any input argument is NULL; the result is NULL.
    pub strict: bool,
}

impl CreateFunction {
    /// Number of call-signature parameters: `IN` + `INOUT` for
    /// functions; every non-TABLE parameter for procedures (callers
    /// pass placeholder arguments for procedure `OUT` parameters,
    /// matching `PostgreSQL` 14+).
    pub fn signature_arity(&self) -> usize {
        self.params
            .iter()
            .filter(|p| self.is_signature_param(p))
            .count()
    }

    /// Number of signature parameters without a `DEFAULT`.
    pub fn required_arity(&self) -> usize {
        self.params
            .iter()
            .filter(|p| self.is_signature_param(p) && p.default.is_none())
            .count()
    }

    fn is_signature_param(&self, p: &FunctionParam) -> bool {
        match p.mode {
            FunctionParamMode::In | FunctionParamMode::InOut => true,
            FunctionParamMode::Out => self.is_procedure,
            FunctionParamMode::Table => false,
        }
    }

    /// Signature parameters in declaration order.
    pub fn signature_params(&self) -> Vec<&FunctionParam> {
        self.params
            .iter()
            .filter(|p| self.is_signature_param(p))
            .collect()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlterTableStmt {
    pub table: String,
    /// Local SQL relation identifier used while binding new or replaced generation expressions.
    pub qualifier: String,
    pub if_exists: bool,
    pub action: AlterTableAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum AlterTableAction {
    AddColumn {
        column: ColumnDef,
        if_not_exists: bool,
    },
    AddKeyConstraint {
        constraint: TableKeyConstraint,
    },
    DropColumn {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    RenameColumn {
        from: String,
        to: String,
    },
    RenameTable {
        to: String,
    },
    SetDefault {
        name: String,
        default: Expr,
    },
    DropDefault {
        name: String,
    },
    SetExpression {
        name: String,
        expression: Expr,
    },
    DropExpression {
        name: String,
    },
    SetNotNull {
        name: String,
    },
    DropNotNull {
        name: String,
    },
    AlterColumnType {
        name: String,
        ty: ColumnType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        using: Option<Expr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertStmt {
    pub table: String,
    /// SQL-visible target relation name: explicit alias, otherwise the local relation name.
    pub target_qualifier: String,
    pub columns: Vec<String>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// Inline `VALUES (...) (...)` rows. Empty when the statement is
    /// an `INSERT ... SELECT` form; in that case `select_source` is
    /// populated with the underlying SELECT.
    pub rows: Vec<Vec<ValueExpr>>,
    /// Populated when the statement is `INSERT INTO t (...) SELECT ...`.
    /// The engine materialises the inner select first and then writes
    /// each row through the standard INSERT path.
    pub select_source: Option<Box<SelectStmt>>,
    /// `ON CONFLICT (...) DO ...` clause. `None` for plain
    /// `INSERT INTO ... VALUES ...` without conflict handling.
    pub on_conflict: Option<OnConflict>,
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    /// `PostgreSQL` 18 names for the old and new row images visible to
    /// `RETURNING`. The defaults are `old` and `new`.
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturningAliases {
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub old_explicit: bool,
    #[serde(default)]
    pub new_explicit: bool,
}

impl Default for ReturningAliases {
    fn default() -> Self {
        Self {
            old: "old".into(),
            new: "new".into(),
            old_explicit: false,
            new_explicit: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnConflict {
    /// Conflict target columns parsed from the `ON CONFLICT (col, ...)`
    /// list. Empty when the clause uses `ON CONFLICT DO NOTHING` with
    /// no target.
    pub conflict_columns: Vec<String>,
    pub action: OnConflictAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OnConflictAction {
    /// `DO NOTHING` -- skip conflicting rows silently.
    Nothing,
    /// `DO UPDATE SET col = expr [, ...] [WHERE pred]` -- apply the
    /// listed assignments to the existing row when the conflict
    /// target matches.
    Update {
        assignments: Vec<(String, Expr)>,
        r#where: Option<Expr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectStmt {
    pub projections: Vec<Projection>,
    /// Rows owned by a `VALUES` query body. `PostgreSQL` represents `VALUES`
    /// through the same query node used for `SELECT`, so nested query bodies
    /// such as CTEs and set-operation branches must retain them here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Vec<Expr>>,
    pub from: Option<FromClause>,
    pub r#where: Option<Expr>,
    pub group_by: Vec<Expr>,
    /// Expanded GROUPING SETS / ROLLUP / CUBE specification. When
    /// non-empty the executor produces one row per grouping set;
    /// `group_by` is treated as a single grouping set in that case.
    /// Each inner Vec lists the grouping-key expressions for that
    /// set (an empty inner Vec means the global grand-total bucket).
    pub grouping_sets: Vec<Vec<Expr>>,
    /// `GROUP BY DISTINCT` -- remove duplicate grouping sets after grouping expressions have been resolved against their input types.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub group_distinct: bool,
    /// `HAVING <expr>`. Evaluated against each aggregated row and
    /// filters out groups whose predicate is falsy. Mirrors PG's
    /// `havingClause`.
    pub having: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `LIMIT <expr>`. Stored as an expression so `LIMIT $1` and any
    /// other constant-folding integer expression resolves at execute
    /// time. `None` means no LIMIT clause was supplied.
    pub limit: Option<Expr>,
    /// `FETCH ... WITH TIES`. The row-count expression remains in [`Self::limit`]; this flag extends the boundary through every row whose complete `ORDER BY` key equals the last requested row.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_ties: bool,
    /// `OFFSET <expr>`. Same shape as [`SelectStmt::limit`].
    pub offset: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// Optional set operation: `Some` for UNION / INTERSECT / EXCEPT.
    /// Parsed statements carry both operands in [`SetOp`]; `left` remains
    /// optional only for backward-compatible deserialization.
    pub set_op: Option<Box<SetOp>>,
    /// `SELECT DISTINCT` -- de-duplicate the final result rows. Set by
    /// the compiler whenever the parsed `distinct_clause` is non-empty.
    pub distinct: bool,
    /// `SELECT DISTINCT ON (<expr>, ...)` keys. Empty for plain
    /// `SELECT DISTINCT`.
    pub distinct_on: Vec<Expr>,
    /// `FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE }` row-locking clauses, in source order. Empty when the query does not lock rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locking: Vec<LockingClause>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CTE {
    pub name: String,
    pub columns: Vec<String>,
    pub recursive: bool,
    pub query: Box<SelectStmt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetOp {
    pub kind: SetOpKind,
    pub all: bool,
    /// Explicit left-hand subtree. Parsed set operations are left-associative,
    /// so a chain such as `a UNION b UNION c` carries `(a UNION b)` here
    /// instead of flattening it back to only `a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Box<SelectStmt>>,
    pub right: SelectStmt,
    /// `ORDER BY` applied to the combined `lhs <op> rhs` result.
    /// Distinct from the LHS / RHS branches' own `ORDER BY`.
    pub combined_order_by: Vec<OrderBy>,
    /// `LIMIT` applied to the combined result. `None` means no
    /// outer LIMIT clause was supplied.
    pub combined_limit: Option<Expr>,
    /// Whether the combined set-operation limit is `FETCH ... WITH TIES`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub combined_with_ties: bool,
    /// `OFFSET` applied to the combined result.
    pub combined_offset: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FromClause {
    /// `FROM <table> [AS <alias>]`.
    Table {
        /// Durable catalog identity, including an explicit schema when present.
        name: String,
        /// Relation name visible to SQL column binding before an alias is applied.
        qualifier: String,
        alias: Option<String>,
    },
    /// `FROM left <kind> right ON predicate`. `lateral` is true when
    /// the right side is a LATERAL subquery / function -- the engine
    /// re-evaluates it for every left row.
    Join {
        left: Box<FromClause>,
        right: Box<FromClause>,
        kind: JoinKind,
        /// Boolean qualification supplied by `ON`. This is mutually
        /// exclusive with `using` and `natural` in parser-produced trees.
        on: Option<Expr>,
        /// `PostgreSQL` `USING (column, ...) [AS alias]` metadata. The column
        /// list must remain explicit until both input row types are known so
        /// binding can validate each side and construct the merged output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        using: Option<JoinUsing>,
        /// `NATURAL` derives its `USING` list from the visible columns of both
        /// input row types at binding time.
        #[serde(default)]
        natural: bool,
        /// Alias applied to the complete parenthesized JOIN result. When
        /// present, the input relation names are hidden from the enclosing
        /// query level.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        /// Positional aliases for the JOIN output after USING/NATURAL shaping.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        column_aliases: Vec<String>,
        #[allow(dead_code)]
        lateral: bool,
    },
    /// `FROM (VALUES (...)...) [AS <alias>(<col_aliases>)]`.
    Values {
        rows: Vec<Vec<Expr>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
    /// `FROM <fn>(<args>) [AS <alias>(<col_aliases>)]` -- e.g.
    /// `generate_series(1, 5)`, `unnest(arr)`, `regexp_split_to_table`,
    /// `json_each(...)`, `cypher(...) AS (col agtype, ...)`. The engine
    /// dispatches by name.
    Function {
        name: String,
        /// Local function identifier used as `PostgreSQL`'s default output column label. Kept separate from the catalog-qualified lookup name so quoted identifiers containing `.` remain indivisible.
        output_name: String,
        /// Catalog relation bound to a relation-aware table function.
        /// Kept separate from scalar arguments so name resolution,
        /// dependency tracking, and planning never treat it as text data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation: Option<String>,
        args: Vec<Expr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        /// Append `PostgreSQL`'s one-based `bigint` ordinality column after the function's ordinary output columns.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ordinality: bool,
        /// Declared column types when the alias used a column
        /// definition list (`AS (col agtype, n int)`); empty when the
        /// alias only renamed columns. Type names are lowercased
        /// `PostgreSQL` internal names (`agtype`, `int4`, `text`, ...).
        #[serde(default)]
        column_types: Vec<String>,
    },
    /// `FROM (SELECT ...) AS <alias>` -- subquery as a relation.
    /// The body re-runs as if a CTE; the alias renames the result
    /// columns when supplied.
    Subquery {
        body: Box<SelectStmt>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinUsing {
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl FromClause {
    /// All table names referenced under this clause, in declaration
    /// order. Used by the compiler to resolve unqualified column refs.
    pub fn collect_tables(&self, out: &mut Vec<(String, Option<String>)>) {
        match self {
            FromClause::Table {
                name,
                qualifier,
                alias,
            } => out.push((
                name.clone(),
                Some(alias.as_ref().unwrap_or(qualifier).clone()),
            )),
            FromClause::Join { left, right, .. } => {
                left.collect_tables(out);
                right.collect_tables(out);
            }
            FromClause::Values { alias, .. }
            | FromClause::Function { alias, .. }
            | FromClause::Subquery { alias, .. } => {
                if let Some(a) = alias {
                    out.push((a.clone(), Some(a.clone())));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

/// `DISCARD` target. Mirrors `PostgreSQL`'s `DiscardMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscardTarget {
    All,
    Plans,
    Sequences,
    Temp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStmt {
    pub table: String,
    pub target_qualifier: String,
    pub assignments: Vec<(String, Expr)>,
    pub r#where: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// `UPDATE t SET ... FROM other [JOIN ...]` -- the engine joins
    /// the target with this clause before applying the assignments.
    pub from: Option<FromClause>,
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteStmt {
    pub table: String,
    pub target_qualifier: String,
    pub r#where: Option<Expr>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// `DELETE FROM t USING other [JOIN ...]` -- the engine joins
    /// the target with this clause and deletes target rows whose
    /// joined image satisfies WHERE.
    pub using: Option<FromClause>,
    /// `RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    Insert(InsertStmt),
    /// `SelectStmt` is the largest variant by far (CTEs + set-ops + n-ary
    /// expression trees), so we box it to keep the enum's stack footprint
    /// proportional to the smaller variants.
    Select(Box<SelectStmt>),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    Drop(DropStmt),
    AlterTable(AlterTableStmt),
    /// `CREATE [OR REPLACE] VIEW name [(column_name, ...)] AS SELECT ...`. The body is the underlying `SelectStmt`; views are materialised lazily on every reference (no row caching).
    CreateView {
        name: String,
        #[serde(default)]
        column_names: Vec<String>,
        body: Box<SelectStmt>,
        or_replace: bool,
    },
    /// `CREATE SCHEMA [IF NOT EXISTS] name`. This AST entry records the
    /// command for the engine's durable schema catalog and namespace
    /// resolver.
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    /// `SET <name> [TO|=] <value>` - runtime parameter assignment.
    /// The engine gives `search_path` resolution semantics and stores other
    /// parameters in the logical session for subsequent `SHOW` statements.
    SetVariable {
        name: String,
        value: String,
    },
    /// `SHOW <variable>` - return the runtime parameter as one
    /// `(name -> value)` row.
    ShowVariable {
        name: String,
    },
    /// `DISCARD [ALL|PLANS|SEQUENCES|TEMP|TEMPORARY]` - clear session state.
    /// The engine resets
    /// session vars, prepared statements and sequences. `TEMP` is rejected
    /// until temporary tables are supported instead of being silently ignored.
    Discard {
        target: DiscardTarget,
    },
    /// `LOAD 'library'` - load a shared library into the session. The
    /// engine embeds its extension surface, so libraries it provides
    /// natively (Apache AGE) load as no-ops and unknown libraries fail
    /// like a missing `$libdir` file.
    Load {
        library: String,
    },
    /// `EXPLAIN ...`. Carries the inner statement so the engine can
    /// emit the planner output.
    Explain {
        analyze: bool,
        verbose: bool,
        format: Option<String>,
        body: Box<Statement>,
    },
    /// `ANALYZE [table]`. The engine refreshes per-column statistics
    /// for cardinality estimation; the AST simply records the target.
    Analyze {
        table: Option<String>,
    },
    /// `TRUNCATE TABLE t1, t2 ...`. Wipes the listed tables.
    Truncate {
        tables: Vec<String>,
        cascade: bool,
    },
    /// `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT name`.
    Transaction(TransactionStmt),
    /// `CREATE SEQUENCE name [START n] [INCREMENT n]`.
    CreateSequence(CreateSequence),
    /// `ALTER SEQUENCE name [RESTART [WITH n]] [INCREMENT [BY] n]
    /// [START [WITH] n]`.
    AlterSequence(AlterSequence),
    /// `CREATE TABLE name AS SELECT ...`.
    CreateTableAs {
        name: String,
        if_not_exists: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        column_names: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        with_no_data: bool,
        body: Box<SelectStmt>,
    },
    /// `PREPARE name AS <inner>`.
    Prepare {
        name: String,
        body: Box<Statement>,
    },
    /// `EXECUTE name (param1, param2, ...)`.
    Execute {
        name: String,
        params: Vec<Expr>,
    },
    /// `DEALLOCATE name | DEALLOCATE ALL`. `None` means ALL.
    Deallocate {
        name: Option<String>,
    },
    /// `SELECT * FROM (VALUES ...) [AS alias]` -- a standalone VALUES
    /// statement (also reachable from a SET-OP body).
    Values {
        rows: Vec<Vec<Expr>>,
    },
    /// `CREATE SERVER name FOREIGN DATA WRAPPER type OPTIONS (...)`.
    CreateForeignServer(CreateForeignServer),
    /// `CREATE FOREIGN TABLE name (...) SERVER server OPTIONS (...)`.
    CreateForeignTable(CreateForeignTable),
    /// `MERGE INTO target USING source ON cond WHEN MATCHED THEN ...
    /// WHEN NOT MATCHED THEN ...`. SQL:2003 conditional UPSERT.
    Merge(MergeStmt),
    /// `CREATE [OR REPLACE] FUNCTION | PROCEDURE ...`. Boxed: the
    /// definition (parameters + body source) dwarfs other variants.
    CreateFunction(Box<CreateFunction>),
    /// `DROP FUNCTION | PROCEDURE [IF EXISTS] name[(args)] [, ...]`.
    DropFunction(DropFunctionStmt),
    /// `DO [LANGUAGE lang] $$ ... $$` - anonymous code block.
    DoBlock {
        language: String,
        body: String,
    },
    /// `CALL proc(args)` - procedure invocation. `OUT` / `INOUT`
    /// parameters shape the result row.
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeStmt {
    pub target: String,
    pub target_qualifier: String,
    pub target_alias: Option<String>,
    pub source: FromClause,
    pub join_condition: Expr,
    pub when_clauses: Vec<MergeWhen>,
    /// `MERGE ... RETURNING ...` projection list. Empty when absent.
    pub returning: Vec<Projection>,
    pub returning_aliases: ReturningAliases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeWhen {
    /// `WHEN MATCHED [AND <cond>] THEN UPDATE SET ...`.
    UpdateMatched {
        condition: Option<Expr>,
        assignments: Vec<(String, Expr)>,
    },
    /// `WHEN MATCHED [AND <cond>] THEN DELETE`.
    DeleteMatched { condition: Option<Expr> },
    /// `WHEN NOT MATCHED [AND <cond>] THEN INSERT (cols) VALUES (vals)`.
    InsertNotMatched {
        condition: Option<Expr>,
        columns: Vec<String>,
        values: Vec<Expr>,
    },
    /// `WHEN MATCHED [AND <cond>] THEN DO NOTHING`.
    NothingMatched { condition: Option<Expr> },
    /// `WHEN NOT MATCHED [AND <cond>] THEN DO NOTHING`.
    NothingNotMatched { condition: Option<Expr> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateForeignServer {
    pub name: String,
    pub fdw_type: String,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateForeignTable {
    pub name: String,
    pub server_name: String,
    pub columns: Vec<ColumnDef>,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSequence {
    pub name: String,
    pub if_not_exists: bool,
    pub start: i64,
    pub increment: i64,
}

/// Physical restart action carried by `ALTER SEQUENCE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceRestart {
    /// No `RESTART` clause was specified.
    #[default]
    Unchanged,
    /// Bare `RESTART`; allocate the configured start value next.
    FromStart,
    /// `RESTART WITH value`; allocate the supplied value next.
    With(i64),
}

fn deserialize_sequence_restart<'de, D>(deserializer: D) -> Result<SequenceRestart, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    enum Current {
        Unchanged,
        FromStart,
        With(i64),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Representation {
        Current(Current),
        // Before SequenceRestart existed this field was
        // Option<Option<i64>>, serialized as null or an integer.
        Legacy(Option<i64>),
    }

    Ok(match Representation::deserialize(deserializer)? {
        Representation::Current(Current::Unchanged) | Representation::Legacy(None) => {
            SequenceRestart::Unchanged
        }
        Representation::Current(Current::FromStart) => SequenceRestart::FromStart,
        Representation::Current(Current::With(value)) | Representation::Legacy(Some(value)) => {
            SequenceRestart::With(value)
        }
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlterSequence {
    pub name: String,
    /// `ALTER SEQUENCE IF EXISTS` suppresses only a missing sequence.
    #[serde(default)]
    pub if_exists: bool,
    /// `RESTART [WITH n]`, preserving omitted, bare, and explicit forms.
    #[serde(default, deserialize_with = "deserialize_sequence_restart")]
    pub restart: SequenceRestart,
    pub increment: Option<i64>,
    pub start: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransactionStmt {
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackToSavepoint(String),
}

#[cfg(test)]
mod tests;
