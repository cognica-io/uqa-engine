//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type-value constructors admit recursive result payloads before copying them.

use super::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

impl ColumnType {
    pub fn array_with_control(
        element: Produced<Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let box_memory = control.reserve(size_of::<Self>())?;
        let (element, memory) = element.into_parts();
        control.finish(
            Self::Array(Box::new(element)),
            control.combine(memory, box_memory),
        )
    }

    /// Copy a domain's names and base type, charging the boxed base.
    pub(crate) fn domain_with_control(
        schema: &str,
        name: &str,
        oid: u32,
        array_oid: Option<u32>,
        collation_oid: Option<u32>,
        base: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        Self::domain_from_base_with_control(
            schema,
            name,
            oid,
            array_oid,
            collation_oid,
            base.clone_with_control(control)?,
            control,
        )
    }

    fn domain_from_base_with_control(
        schema: &str,
        name: &str,
        oid: u32,
        array_oid: Option<u32>,
        collation_oid: Option<u32>,
        base: Produced<Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let schema = control.copy_text(schema)?;
        let name = control.copy_text(name)?;
        let box_memory = control.reserve(size_of::<Self>())?;
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        let (base, base_memory) = base.into_parts();
        let memory = control.combine(
            control.combine(schema_memory, name_memory),
            control.combine(base_memory, box_memory),
        );
        control.finish(
            Self::Domain {
                schema,
                name,
                oid,
                array_oid,
                collation_oid,
                base: Box::new(base),
            },
            memory,
        )
    }

    /// Copy unary ownership chains in source order without recursive stack frames or scratch allocation.
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        // On failure the partial value retires before its accumulated payload lease.
        let mut memory = control.empty_reservation();
        let mut value = Self::Void;
        let mut target = &mut value;
        let mut source = self;
        loop {
            let (node, node_memory) = source.clone_node_with_control(control)?.into_parts();
            memory = control.combine(memory, node_memory);
            *target = node;
            match (source, target) {
                (Self::Array(child), Self::Array(next))
                | (Self::Domain { base: child, .. }, Self::Domain { base: next, .. }) => {
                    source = child;
                    target = next;
                }
                _ => return control.finish(value, memory),
            }
        }
    }

    /// Copy only owned type payloads; inline scalar identities require no reservation.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive type retention keeps every payload-bearing variant charged to its owner"
    )]
    fn clone_node_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        match self {
            Self::Named(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                control.finish(Self::Named(name), memory)
            }
            Self::Array(_) => Self::array_with_control(
                control.finish(Self::Void, control.empty_reservation())?,
                control,
            ),
            Self::Enum(reference) => {
                let (reference, memory) = reference.clone_with_control(control)?.into_parts();
                control.finish(Self::Enum(reference), memory)
            }
            Self::Composite(reference) => {
                let (reference, memory) = reference.clone_with_control(control)?.into_parts();
                control.finish(Self::Composite(reference), memory)
            }
            Self::DeclaredRange(reference) | Self::DeclaredMultirange(reference) => {
                let (reference, memory) = reference.clone_with_control(control)?.into_parts();
                let ty = if matches!(self, Self::DeclaredRange(_)) {
                    Self::DeclaredRange(reference)
                } else {
                    Self::DeclaredMultirange(reference)
                };
                control.finish(ty, memory)
            }
            Self::Domain {
                schema,
                name,
                oid,
                array_oid,
                collation_oid,
                base: _,
            } => Self::domain_from_base_with_control(
                schema,
                name,
                *oid,
                *array_oid,
                *collation_oid,
                control.finish(Self::Void, control.empty_reservation())?,
                control,
            ),
            other => {
                let scalar = match other {
                    Self::SmallInteger => Self::SmallInteger,
                    Self::Integer => Self::Integer,
                    Self::BigInteger => Self::BigInteger,
                    Self::Oid => Self::Oid,
                    Self::Xid => Self::Xid,
                    Self::Boolean => Self::Boolean,
                    Self::Void => Self::Void,
                    Self::Text => Self::Text,
                    Self::RefCursor => Self::RefCursor,
                    Self::Name => Self::Name,
                    Self::Uuid => Self::Uuid,
                    Self::Bpchar => Self::Bpchar,
                    Self::Real => Self::Real,
                    Self::DoublePrecision => Self::DoublePrecision,
                    Self::Json => Self::Json,
                    Self::Xml => Self::Xml,
                    Self::JsonB => Self::JsonB,
                    Self::Bytea => Self::Bytea,
                    Self::InternalChar => Self::InternalChar,
                    Self::Regproc => Self::Regproc,
                    Self::Regprocedure => Self::Regprocedure,
                    Self::Regclass => Self::Regclass,
                    Self::Regcollation => Self::Regcollation,
                    Self::Regnamespace => Self::Regnamespace,
                    Self::Regrole => Self::Regrole,
                    Self::Regtype => Self::Regtype,
                    Self::PgNodeTree => Self::PgNodeTree,
                    Self::AclItem => Self::AclItem,
                    Self::Int2Vector => Self::Int2Vector,
                    Self::OidVector => Self::OidVector,
                    Self::AnyArray => Self::AnyArray,
                    Self::Record => Self::Record,
                    Self::Date => Self::Date,
                    Self::Time => Self::Time,
                    Self::TimeTz => Self::TimeTz,
                    Self::Timestamp => Self::Timestamp,
                    Self::TimestampTz => Self::TimestampTz,
                    Self::Interval => Self::Interval,
                    Self::Varchar(value) => Self::Varchar(*value),
                    Self::Character(value) => Self::Character(*value),
                    Self::Bit(value) => Self::Bit(*value),
                    Self::VarBit(value) => Self::VarBit(*value),
                    Self::TimePrecision(value) => Self::TimePrecision(*value),
                    Self::TimeTzPrecision(value) => Self::TimeTzPrecision(*value),
                    Self::TimestampPrecision(value) => Self::TimestampPrecision(*value),
                    Self::TimestampTzPrecision(value) => Self::TimestampTzPrecision(*value),
                    Self::Range(value) => Self::Range(*value),
                    Self::Multirange(value) => Self::Multirange(*value),
                    Self::Vector(value) => Self::Vector(*value),
                    Self::Tensor(value) => Self::Tensor(*value),
                    Self::Numeric { precision, scale } => Self::Numeric {
                        precision: *precision,
                        scale: *scale,
                    },
                    Self::IntervalWithFields { fields, precision } => Self::IntervalWithFields {
                        fields: *fields,
                        precision: *precision,
                    },
                    Self::Named(_)
                    | Self::Array(_)
                    | Self::Domain { .. }
                    | Self::Enum(_)
                    | Self::Composite(_)
                    | Self::DeclaredRange(_)
                    | Self::DeclaredMultirange(_) => unreachable!("owned payload copied above"),
                };
                control.finish(scalar, control.empty_reservation())
            }
        }
    }

    pub fn without_type_modifiers_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let mut dimensions = 0;
        let mut element = self;
        while let Self::Array(child) = element {
            control.check()?;
            dimensions += 1;
            element = child;
        }
        let scalar = match element {
            Self::Varchar(_) => Some(Self::Varchar(None)),
            Self::Bit(_) => Some(Self::Bit(None)),
            Self::VarBit(_) => Some(Self::VarBit(None)),
            Self::Character(_) => Some(Self::Bpchar),
            Self::Numeric { .. } => Some(Self::Numeric {
                precision: None,
                scale: None,
            }),
            _ => None,
        };
        let mut output = match scalar {
            Some(scalar) => control.finish(scalar, control.empty_reservation())?,
            None => element
                .without_temporal_modifiers()
                .clone_with_control(control)?,
        };
        for _ in 0..dimensions {
            output = Self::array_with_control(output, control)?;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
