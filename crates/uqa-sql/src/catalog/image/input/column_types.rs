//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native type decoding admits payload and box layouts before construction; unary chains need no recursive decoder or traversal workspace.

use super::super::column_types::TypeTag;
use super::{invalid, Fields, Input};
use crate::{
    ast::{
        ColumnType, CompositeTypeReference, EnumTypeReference, IntervalFields, RangeAlignment,
        RangeSubtype, RangeTypeReference,
    },
    SQLError,
};
use uqa_core::memory::{Budgeted, MemoryReservation};

impl Fields<'_> {
    async fn type_text(&mut self, reservation: &mut MemoryReservation) -> Result<String, SQLError> {
        let (text, memory) = self.binary.text().await?.into_parts();
        reservation.absorb(memory);
        if text.is_empty() || text.contains('\0') {
            return Err(invalid("column type name"));
        }
        Ok(text)
    }
    pub(super) async fn optional_number(&mut self) -> Result<Option<u32>, SQLError> {
        if self.flag().await? {
            Ok(Some(u32::from_le_bytes(self.binary.array().await?)))
        } else {
            Ok(None)
        }
    }
    pub(super) async fn column_type(&mut self) -> Result<Budgeted<ColumnType>, SQLError> {
        let mut reservation = self.binary.memory.empty_reservation();
        let mut value = ColumnType::Void;
        let mut target = &mut value;
        loop {
            self.binary.cancellation.check()?;
            let tag = TypeTag::decode(self.binary.array::<1>().await?[0])?;
            *target = self.type_node(tag, &mut reservation).await?;
            match target {
                ColumnType::Array(next) | ColumnType::Domain { base: next, .. } => target = next,
                _ => return Ok(Budgeted::new(value, reservation)),
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive versioned reconstruction keeps each native payload and admission explicit"
    )]
    async fn type_node(
        &mut self,
        tag: TypeTag,
        reservation: &mut MemoryReservation,
    ) -> Result<ColumnType, SQLError> {
        Ok(match tag {
            TypeTag::SmallInteger => ColumnType::SmallInteger,
            TypeTag::Integer => ColumnType::Integer,
            TypeTag::BigInteger => ColumnType::BigInteger,
            TypeTag::Oid => ColumnType::Oid,
            TypeTag::Xid => ColumnType::Xid,
            TypeTag::Boolean => ColumnType::Boolean,
            TypeTag::Void => ColumnType::Void,
            TypeTag::Text => ColumnType::Text,
            TypeTag::RefCursor => ColumnType::RefCursor,
            TypeTag::Name => ColumnType::Name,
            TypeTag::Uuid => ColumnType::Uuid,
            TypeTag::Bpchar => ColumnType::Bpchar,
            TypeTag::Real => ColumnType::Real,
            TypeTag::DoublePrecision => ColumnType::DoublePrecision,
            TypeTag::Json => ColumnType::Json,
            TypeTag::Xml => ColumnType::Xml,
            TypeTag::JsonB => ColumnType::JsonB,
            TypeTag::Bytea => ColumnType::Bytea,
            TypeTag::InternalChar => ColumnType::InternalChar,
            TypeTag::Regproc => ColumnType::Regproc,
            TypeTag::Regprocedure => ColumnType::Regprocedure,
            TypeTag::Regclass => ColumnType::Regclass,
            TypeTag::Regnamespace => ColumnType::Regnamespace,
            TypeTag::Regcollation => ColumnType::Regcollation,
            TypeTag::Regrole => ColumnType::Regrole,
            TypeTag::Regtype => ColumnType::Regtype,
            TypeTag::PgNodeTree => ColumnType::PgNodeTree,
            TypeTag::AclItem => ColumnType::AclItem,
            TypeTag::Int2Vector => ColumnType::Int2Vector,
            TypeTag::OidVector => ColumnType::OidVector,
            TypeTag::AnyArray => ColumnType::AnyArray,
            TypeTag::Record => ColumnType::Record,
            TypeTag::Date => ColumnType::Date,
            TypeTag::Time => ColumnType::Time,
            TypeTag::TimeTz => ColumnType::TimeTz,
            TypeTag::Timestamp => ColumnType::Timestamp,
            TypeTag::TimestampTz => ColumnType::TimestampTz,
            TypeTag::Interval => ColumnType::Interval,
            TypeTag::Named => return Err(invalid("unresolved stored column type")),
            TypeTag::Varchar => ColumnType::Varchar(self.optional_number().await?),
            TypeTag::Bit => ColumnType::Bit(self.optional_number().await?),
            TypeTag::VarBit => ColumnType::VarBit(self.optional_number().await?),
            TypeTag::Character => {
                ColumnType::Character(u32::from_le_bytes(self.binary.array().await?))
            }
            TypeTag::TimePrecision => {
                ColumnType::TimePrecision(u32::from_le_bytes(self.binary.array().await?))
            }
            TypeTag::TimeTzPrecision => {
                ColumnType::TimeTzPrecision(u32::from_le_bytes(self.binary.array().await?))
            }
            TypeTag::TimestampPrecision => {
                ColumnType::TimestampPrecision(u32::from_le_bytes(self.binary.array().await?))
            }
            TypeTag::TimestampTzPrecision => {
                ColumnType::TimestampTzPrecision(u32::from_le_bytes(self.binary.array().await?))
            }
            TypeTag::Vector => ColumnType::Vector(u32::from_le_bytes(self.binary.array().await?)),
            TypeTag::Tensor => ColumnType::Tensor(u32::from_le_bytes(self.binary.array().await?)),
            TypeTag::Numeric => ColumnType::Numeric {
                precision: self.optional_number().await?,
                scale: if self.flag().await? {
                    Some(i32::from_le_bytes(self.binary.array().await?))
                } else {
                    None
                },
            },
            TypeTag::Array => {
                reservation.grow(size_of::<ColumnType>())?;
                ColumnType::Array(Box::new(ColumnType::Void))
            }
            TypeTag::IntervalWithFields => ColumnType::IntervalWithFields {
                fields: IntervalFields::from_modifier_mask(i64::from(u16::from_le_bytes(
                    self.binary.array().await?,
                )))
                .ok_or_else(|| invalid("interval field mask"))?,
                precision: self.optional_number().await?,
            },
            TypeTag::Range | TypeTag::Multirange => {
                let subtype = match self.binary.array::<1>().await?[0] {
                    0 => RangeSubtype::Integer,
                    1 => RangeSubtype::BigInteger,
                    2 => RangeSubtype::Numeric,
                    3 => RangeSubtype::Date,
                    4 => RangeSubtype::Timestamp,
                    5 => RangeSubtype::TimestampTz,
                    _ => return Err(invalid("range subtype")),
                };
                if matches!(tag, TypeTag::Range) {
                    ColumnType::Range(subtype)
                } else {
                    ColumnType::Multirange(subtype)
                }
            }
            TypeTag::DeclaredRange | TypeTag::DeclaredMultirange => {
                let reference = RangeTypeReference {
                    schema: self.type_text(reservation).await?,
                    name: self.type_text(reservation).await?,
                    oid: self.catalog_oid().await?,
                    array_oid: self.catalog_oid().await?,
                    alignment: match self.binary.array::<1>().await?[0] {
                        0 => RangeAlignment::Integer,
                        1 => RangeAlignment::Double,
                        _ => return Err(invalid("range alignment")),
                    },
                };
                if matches!(tag, TypeTag::DeclaredRange) {
                    ColumnType::DeclaredRange(reference)
                } else {
                    ColumnType::DeclaredMultirange(reference)
                }
            }
            TypeTag::Domain => {
                let schema = self.type_text(reservation).await?;
                let name = self.type_text(reservation).await?;
                let oid = self.catalog_oid().await?;
                let array_oid = self.optional_oid().await?;
                let collation_oid = self.optional_number().await?;
                reservation.grow(size_of::<ColumnType>())?;
                ColumnType::Domain {
                    schema,
                    name,
                    oid,
                    array_oid,
                    collation_oid,
                    base: Box::new(ColumnType::Void),
                }
            }
            TypeTag::Enum => ColumnType::Enum(EnumTypeReference {
                schema: self.type_text(reservation).await?,
                name: self.type_text(reservation).await?,
                oid: self.catalog_oid().await?,
                array_oid: self.catalog_oid().await?,
            }),
            TypeTag::Composite => ColumnType::Composite(CompositeTypeReference {
                schema: self.type_text(reservation).await?,
                name: self.type_text(reservation).await?,
                oid: self.catalog_oid().await?,
                array_oid: self.catalog_oid().await?,
                relation_oid: self.catalog_oid().await?,
            }),
        })
    }
}

impl Input<'_> {
    pub(super) fn optional_number(&mut self) -> Result<Option<u32>, SQLError> {
        self.fields(async |mut input| input.optional_number().await)
    }
    pub(super) fn column_type(&mut self) -> Result<Budgeted<ColumnType>, SQLError> {
        self.fields(async |mut input| input.column_type().await)
    }
}
