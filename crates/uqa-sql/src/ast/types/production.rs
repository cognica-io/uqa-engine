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

    /// Copy only owned type payloads; inline scalar identities require no reservation.
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        match self {
            Self::Named(name) => {
                let (name, memory) = control.copy_text(name)?.into_parts();
                control.finish(Self::Named(name), memory)
            }
            Self::Array(element) => {
                Self::array_with_control(element.clone_with_control(control)?, control)
            }
            Self::Domain {
                schema,
                name,
                oid,
                base,
            } => {
                let schema = control.copy_text(schema)?;
                let name = control.copy_text(name)?;
                let base = base.clone_with_control(control)?;
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
                        oid: *oid,
                        base: Box::new(base),
                    },
                    memory,
                )
            }
            Self::SmallInteger
            | Self::Integer
            | Self::BigInteger
            | Self::Oid
            | Self::Xid
            | Self::Boolean
            | Self::Void
            | Self::Text
            | Self::RefCursor
            | Self::Name
            | Self::Uuid
            | Self::Varchar(_)
            | Self::Bpchar
            | Self::Character(_)
            | Self::Real
            | Self::DoublePrecision
            | Self::Numeric {
                precision: _,
                scale: _,
            }
            | Self::Json
            | Self::JsonB
            | Self::Bytea
            | Self::InternalChar
            | Self::Regproc
            | Self::Regprocedure
            | Self::Regclass
            | Self::Regnamespace
            | Self::Regrole
            | Self::Regtype
            | Self::PgNodeTree
            | Self::AclItem
            | Self::Int2Vector
            | Self::OidVector
            | Self::AnyArray
            | Self::Record
            | Self::Date
            | Self::Time
            | Self::TimePrecision(_)
            | Self::TimeTz
            | Self::TimeTzPrecision(_)
            | Self::Timestamp
            | Self::TimestampPrecision(_)
            | Self::TimestampTz
            | Self::TimestampTzPrecision(_)
            | Self::Interval
            | Self::IntervalWithFields {
                fields: _,
                precision: _,
            }
            | Self::Range(_)
            | Self::Multirange(_)
            | Self::Vector(_)
            | Self::Tensor(_) => control.finish(self.clone(), control.empty_reservation()),
        }
    }

    pub fn without_type_modifiers_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let scalar = match self {
            Self::Varchar(_) => Self::Varchar(None),
            Self::Character(_) => Self::Bpchar,
            Self::Numeric { .. } => Self::Numeric {
                precision: None,
                scale: None,
            },
            Self::Array(element) => {
                return Self::array_with_control(
                    element.without_type_modifiers_with_control(control)?,
                    control,
                )
            }
            other => {
                return other
                    .without_temporal_modifiers()
                    .clone_with_control(control)
            }
        };
        control.finish(scalar, control.empty_reservation())
    }
}

#[cfg(test)]
mod tests;
