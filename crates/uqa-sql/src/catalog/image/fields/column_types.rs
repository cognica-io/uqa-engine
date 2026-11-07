//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each projection emits one type node and borrows its sole recursive child.

use super::super::column_types::TypeTag;
use super::Fields;
use crate::ast::{
    ColumnType, CompositeTypeReference, EnumTypeReference, RangeAlignment, RangeSubtype,
    RangeTypeReference,
};

impl<'a> Fields<'a> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive versioned projection keeps every native type field explicit"
    )]
    pub fn column_type(&mut self, value: &'a ColumnType) -> Option<&'a ColumnType> {
        match value {
            ColumnType::SmallInteger => self.scalar([TypeTag::SmallInteger as u8]),
            ColumnType::Integer => self.scalar([TypeTag::Integer as u8]),
            ColumnType::BigInteger => self.scalar([TypeTag::BigInteger as u8]),
            ColumnType::Oid => self.scalar([TypeTag::Oid as u8]),
            ColumnType::Xid => self.scalar([TypeTag::Xid as u8]),
            ColumnType::Boolean => self.scalar([TypeTag::Boolean as u8]),
            ColumnType::Void => self.scalar([TypeTag::Void as u8]),
            ColumnType::Text => self.scalar([TypeTag::Text as u8]),
            ColumnType::RefCursor => self.scalar([TypeTag::RefCursor as u8]),
            ColumnType::Name => self.scalar([TypeTag::Name as u8]),
            ColumnType::Uuid => self.scalar([TypeTag::Uuid as u8]),
            ColumnType::Bpchar => self.scalar([TypeTag::Bpchar as u8]),
            ColumnType::Real => self.scalar([TypeTag::Real as u8]),
            ColumnType::DoublePrecision => self.scalar([TypeTag::DoublePrecision as u8]),
            ColumnType::Json => self.scalar([TypeTag::Json as u8]),
            ColumnType::Xml => self.scalar([TypeTag::Xml as u8]),
            ColumnType::JsonB => self.scalar([TypeTag::JsonB as u8]),
            ColumnType::Bytea => self.scalar([TypeTag::Bytea as u8]),
            ColumnType::InternalChar => self.scalar([TypeTag::InternalChar as u8]),
            ColumnType::Regproc => self.scalar([TypeTag::Regproc as u8]),
            ColumnType::Regprocedure => self.scalar([TypeTag::Regprocedure as u8]),
            ColumnType::Regclass => self.scalar([TypeTag::Regclass as u8]),
            ColumnType::Regnamespace => self.scalar([TypeTag::Regnamespace as u8]),
            ColumnType::Regcollation => self.scalar([TypeTag::Regcollation as u8]),
            ColumnType::Regrole => self.scalar([TypeTag::Regrole as u8]),
            ColumnType::Regtype => self.scalar([TypeTag::Regtype as u8]),
            ColumnType::PgNodeTree => self.scalar([TypeTag::PgNodeTree as u8]),
            ColumnType::AclItem => self.scalar([TypeTag::AclItem as u8]),
            ColumnType::Int2Vector => self.scalar([TypeTag::Int2Vector as u8]),
            ColumnType::OidVector => self.scalar([TypeTag::OidVector as u8]),
            ColumnType::AnyArray => self.scalar([TypeTag::AnyArray as u8]),
            ColumnType::Record => self.scalar([TypeTag::Record as u8]),
            ColumnType::Date => self.scalar([TypeTag::Date as u8]),
            ColumnType::Time => self.scalar([TypeTag::Time as u8]),
            ColumnType::TimeTz => self.scalar([TypeTag::TimeTz as u8]),
            ColumnType::Timestamp => self.scalar([TypeTag::Timestamp as u8]),
            ColumnType::TimestampTz => self.scalar([TypeTag::TimestampTz as u8]),
            ColumnType::Interval => self.scalar([TypeTag::Interval as u8]),
            ColumnType::Named(name) => {
                self.scalar([TypeTag::Named as u8]);
                self.text(name);
            }
            ColumnType::Varchar(value) => {
                self.scalar([TypeTag::Varchar as u8]);
                self.optional_u32(*value);
            }
            ColumnType::Bit(value) => {
                self.scalar([TypeTag::Bit as u8]);
                self.optional_u32(*value);
            }
            ColumnType::VarBit(value) => {
                self.scalar([TypeTag::VarBit as u8]);
                self.optional_u32(*value);
            }
            ColumnType::Character(value) => {
                self.scalar([TypeTag::Character as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::TimePrecision(value) => {
                self.scalar([TypeTag::TimePrecision as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::TimeTzPrecision(value) => {
                self.scalar([TypeTag::TimeTzPrecision as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::TimestampPrecision(value) => {
                self.scalar([TypeTag::TimestampPrecision as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::TimestampTzPrecision(value) => {
                self.scalar([TypeTag::TimestampTzPrecision as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::Vector(value) => {
                self.scalar([TypeTag::Vector as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::Tensor(value) => {
                self.scalar([TypeTag::Tensor as u8]);
                self.scalar(value.to_le_bytes());
            }
            ColumnType::Numeric { precision, scale } => {
                self.scalar([TypeTag::Numeric as u8]);
                self.optional_u32(*precision);
                self.flag(scale.is_some());
                if let Some(scale) = scale {
                    self.scalar(scale.to_le_bytes());
                }
            }
            ColumnType::Array(element) => {
                self.scalar([TypeTag::Array as u8]);
                return Some(element);
            }
            ColumnType::IntervalWithFields { fields, precision } => {
                self.scalar([TypeTag::IntervalWithFields as u8]);
                self.scalar(fields.modifier_mask().to_le_bytes());
                self.optional_u32(*precision);
            }
            ColumnType::Range(subtype) | ColumnType::Multirange(subtype) => {
                self.scalar([if matches!(value, ColumnType::Range(_)) {
                    TypeTag::Range
                } else {
                    TypeTag::Multirange
                } as u8]);
                self.scalar([match subtype {
                    RangeSubtype::Integer => 0,
                    RangeSubtype::BigInteger => 1,
                    RangeSubtype::Numeric => 2,
                    RangeSubtype::Date => 3,
                    RangeSubtype::Timestamp => 4,
                    RangeSubtype::TimestampTz => 5,
                }]);
            }
            ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
                self.scalar([if matches!(value, ColumnType::DeclaredRange(_)) {
                    TypeTag::DeclaredRange
                } else {
                    TypeTag::DeclaredMultirange
                } as u8]);
                let RangeTypeReference {
                    schema,
                    name,
                    oid,
                    array_oid,
                    alignment,
                } = reference;
                self.text(schema);
                self.text(name);
                self.scalar(oid.to_le_bytes());
                self.scalar(array_oid.to_le_bytes());
                self.scalar([match alignment {
                    RangeAlignment::Integer => 0,
                    RangeAlignment::Double => 1,
                }]);
            }
            ColumnType::Domain {
                schema,
                name,
                oid,
                array_oid,
                collation_oid,
                base,
            } => {
                self.scalar([TypeTag::Domain as u8]);
                self.text(schema);
                self.text(name);
                self.scalar(oid.to_le_bytes());
                self.optional_u32(*array_oid);
                self.optional_u32(*collation_oid);
                return Some(base);
            }
            ColumnType::Enum(reference) => {
                let EnumTypeReference {
                    schema,
                    name,
                    oid,
                    array_oid,
                } = reference;
                self.scalar([TypeTag::Enum as u8]);
                self.text(schema);
                self.text(name);
                self.scalar(oid.to_le_bytes());
                self.scalar(array_oid.to_le_bytes());
            }
            ColumnType::Composite(reference) => {
                let CompositeTypeReference {
                    schema,
                    name,
                    oid,
                    array_oid,
                    relation_oid,
                } = reference;
                self.scalar([TypeTag::Composite as u8]);
                self.text(schema);
                self.text(name);
                self.scalar(oid.to_le_bytes());
                self.scalar(array_oid.to_le_bytes());
                self.scalar(relation_oid.to_le_bytes());
            }
        }
        None
    }
}
