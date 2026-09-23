//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type names stream into the caller's ordinary or admitted destination without intermediate owned names.

use super::ColumnType;
use std::fmt;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

impl ColumnType {
    #[must_use]
    pub fn sql_name(&self) -> String {
        TypeName {
            ty: self,
            regtype: false,
        }
        .to_string()
    }

    /// Name emitted by `PostgreSQL`'s regtype output, including `pg_typeof`.
    #[must_use]
    pub fn regtype_name(&self) -> String {
        TypeName {
            ty: self,
            regtype: true,
        }
        .to_string()
    }

    pub fn sql_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.format(format_args!(
            "{}",
            TypeName {
                ty: self,
                regtype: false
            }
        ))
    }

    pub fn regtype_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.format(format_args!(
            "{}",
            TypeName {
                ty: self,
                regtype: true
            }
        ))
    }
}

struct TypeName<'a> {
    ty: &'a ColumnType,
    regtype: bool,
}

impl fmt::Display for TypeName<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "one formatter preserves exhaustive SQL and regtype spellings"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.regtype {
            if matches!(self.ty, ColumnType::IntervalWithFields { .. }) {
                return f.write_str("interval");
            }
            if self.ty.temporal_precision().is_some() {
                return TypeName {
                    ty: self.ty.without_temporal_modifiers(),
                    regtype: true,
                }
                .fmt(f);
            }
            match self.ty {
                ColumnType::Varchar(_) => return f.write_str("character varying"),
                ColumnType::Bpchar | ColumnType::Character(_) => return f.write_str("character"),
                ColumnType::Numeric { .. } => return f.write_str("numeric"),
                ColumnType::Vector(_) => return f.write_str("vector"),
                ColumnType::Tensor(_) => return f.write_str("tensor"),
                ColumnType::Array(element) => {
                    return write!(
                        f,
                        "{}[]",
                        TypeName {
                            ty: element,
                            regtype: true
                        }
                    )
                }
                _ => {}
            }
        }
        match self.ty {
            ColumnType::Named(name) => f.write_str(name),
            ColumnType::SmallInteger => f.write_str("smallint"),
            ColumnType::Integer => f.write_str("integer"),
            ColumnType::BigInteger => f.write_str("bigint"),
            ColumnType::Oid => f.write_str("oid"),
            ColumnType::Xid => f.write_str("xid"),
            ColumnType::Boolean => f.write_str("boolean"),
            ColumnType::Void => f.write_str("void"),
            ColumnType::Text => f.write_str("text"),
            ColumnType::RefCursor => f.write_str("refcursor"),
            ColumnType::Name => f.write_str("name"),
            ColumnType::Uuid => f.write_str("uuid"),
            ColumnType::Varchar(Some(length)) => write!(f, "character varying({length})"),
            ColumnType::Varchar(None) => f.write_str("character varying"),
            ColumnType::Bpchar => f.write_str("bpchar"),
            ColumnType::Character(length) => write!(f, "character({length})"),
            ColumnType::Real => f.write_str("real"),
            ColumnType::DoublePrecision => f.write_str("double precision"),
            ColumnType::Numeric {
                precision: Some(precision),
                scale: Some(scale),
            } => write!(f, "numeric({precision},{scale})"),
            ColumnType::Numeric { .. } => f.write_str("numeric"),
            ColumnType::Json => f.write_str("json"),
            ColumnType::JsonB => f.write_str("jsonb"),
            ColumnType::Bytea => f.write_str("bytea"),
            ColumnType::InternalChar => f.write_str("\"char\""),
            ColumnType::Regproc => f.write_str("regproc"),
            ColumnType::Regprocedure => f.write_str("regprocedure"),
            ColumnType::Regclass => f.write_str("regclass"),
            ColumnType::Regnamespace => f.write_str("regnamespace"),
            ColumnType::Regrole => f.write_str("regrole"),
            ColumnType::Regtype => f.write_str("regtype"),
            ColumnType::PgNodeTree => f.write_str("pg_node_tree"),
            ColumnType::AclItem => f.write_str("aclitem"),
            ColumnType::Int2Vector => f.write_str("int2vector"),
            ColumnType::OidVector => f.write_str("oidvector"),
            ColumnType::AnyArray => f.write_str("anyarray"),
            ColumnType::Record => f.write_str("record"),
            ColumnType::Array(element) => write!(
                f,
                "{}[]",
                TypeName {
                    ty: element,
                    regtype: false
                }
            ),
            ColumnType::Date => f.write_str("date"),
            ColumnType::Time => f.write_str("time without time zone"),
            ColumnType::TimePrecision(p) => write!(f, "time({p}) without time zone"),
            ColumnType::TimeTz => f.write_str("time with time zone"),
            ColumnType::TimeTzPrecision(p) => write!(f, "time({p}) with time zone"),
            ColumnType::Timestamp => f.write_str("timestamp without time zone"),
            ColumnType::TimestampPrecision(p) => write!(f, "timestamp({p}) without time zone"),
            ColumnType::TimestampTz => f.write_str("timestamp with time zone"),
            ColumnType::TimestampTzPrecision(p) => write!(f, "timestamp({p}) with time zone"),
            ColumnType::Interval => f.write_str("interval"),
            ColumnType::IntervalWithFields { fields, precision } => {
                write!(f, "interval{}", fields.sql_suffix())?;
                if let Some(precision) = precision {
                    write!(f, "({precision})")?;
                }
                Ok(())
            }
            ColumnType::Range(subtype) => f.write_str(subtype.range_name()),
            ColumnType::Multirange(subtype) => f.write_str(subtype.multirange_name()),
            ColumnType::Vector(dimension) => write!(f, "vector({dimension})"),
            ColumnType::Tensor(dimension) => write!(f, "tensor({dimension})"),
            ColumnType::Domain { schema, name, .. } => {
                crate::compiler::write_relation_component(schema, f)?;
                f.write_str(".")?;
                crate::compiler::write_relation_component(name, f)
            }
        }
    }
}

#[cfg(test)]
mod tests;
