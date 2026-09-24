//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Node, Result, Walker};
use crate::ast::{
    AutoIncrement, AutoIncrementOwner, ColumnDef, ColumnType, ForeignKeyRef, FunctionBinding,
    FunctionResolutionError, GeneratedColumn, OperatorResolutionError, RoutineInvocationBinding,
};

impl<'a> Walker<'a> {
    pub(super) fn column(&mut self, column: &'a ColumnDef) -> Result<()> {
        let ColumnDef {
            name,
            ty,
            object_id: _,
            missing_value,
            primary_key: _,
            not_null: _,
            not_null_explicit: _,
            not_null_name,
            not_null_identity: _,
            not_null_validated: _,
            not_null_no_inherit: _,
            not_null_is_local: _,
            auto_increment,
            unique: _,
            default,
            generated,
            check,
            check_name,
            check_enforced: _,
            check_validated: _,
            check_no_inherit: _,
            check_is_local: _,
            check_object_id: _,
            check_catalog_oid: _,
            references,
        } = column;
        self.text(name)?;
        self.node(Node::Type(ty))?;
        if let Some(value) = missing_value {
            self.value(value)?;
        }
        self.optional_text(not_null_name.as_ref())?;
        self.optional_text(check_name.as_ref())?;
        self.optional_expr(default.as_ref())?;
        self.optional_expr(check.as_ref())?;
        if let Some(AutoIncrement {
            kind: _,
            sequence,
            owner,
        }) = auto_increment
        {
            self.optional_text(sequence.as_ref())?;
            if let Some(AutoIncrementOwner { table, column }) = owner {
                self.text(table)?;
                self.text(column)?;
            }
        }
        if let Some(GeneratedColumn {
            kind: _,
            expression,
            function_dependencies,
        }) = generated
        {
            self.boxed(expression.as_ref(), Node::Expr)?;
            self.children(function_dependencies, Node::Binding)?;
        }
        if let Some(ForeignKeyRef {
            referenced_index: _,
            referenced_key,
            name,
            object_id: _,
            catalog_identity: _,
            table,
            column,
            on_update: _,
            on_delete: _,
            match_type: _,
            enforced: _,
            validated: _,
            deferrable: _,
            initially_deferred: _,
            period: _,
        }) = references
        {
            self.optional_text(referenced_key.as_ref())?;
            self.optional_text(name.as_ref())?;
            self.text(table)?;
            self.optional_text(column.as_ref())?;
        }
        Ok(())
    }

    pub(super) fn ty(&mut self, ty: &'a ColumnType) -> Result<()> {
        match ty {
            ColumnType::Named(name) => self.text(name),
            ColumnType::Array(element) => self.boxed(element.as_ref(), Node::Type),
            ColumnType::Domain {
                schema,
                name,
                oid: _,
                base,
            } => {
                self.text(schema)?;
                self.text(name)?;
                self.boxed(base.as_ref(), Node::Type)
            }
            ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Boolean
            | ColumnType::Void
            | ColumnType::Text
            | ColumnType::RefCursor
            | ColumnType::Name
            | ColumnType::Uuid
            | ColumnType::Varchar(_)
            | ColumnType::Bpchar
            | ColumnType::Character(_)
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric {
                precision: _,
                scale: _,
            }
            | ColumnType::Json
            | ColumnType::JsonB
            | ColumnType::Bytea
            | ColumnType::InternalChar
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
            | ColumnType::PgNodeTree
            | ColumnType::AclItem
            | ColumnType::Int2Vector
            | ColumnType::OidVector
            | ColumnType::AnyArray
            | ColumnType::Record
            | ColumnType::Date
            | ColumnType::Time
            | ColumnType::TimePrecision(_)
            | ColumnType::TimeTz
            | ColumnType::TimeTzPrecision(_)
            | ColumnType::Timestamp
            | ColumnType::TimestampPrecision(_)
            | ColumnType::TimestampTz
            | ColumnType::TimestampTzPrecision(_)
            | ColumnType::Interval
            | ColumnType::IntervalWithFields {
                fields: _,
                precision: _,
            }
            | ColumnType::Range(_)
            | ColumnType::Multirange(_)
            | ColumnType::Vector(_)
            | ColumnType::Tensor(_) => Ok(()),
        }
    }

    pub(super) fn binding(&mut self, binding: &'a FunctionBinding) -> Result<()> {
        let FunctionBinding {
            object_id: _,
            name,
            argument_types,
            builtin: _,
            dispatch: _,
            invocation,
            resolution_error,
        } = binding;
        self.text(name)?;
        self.texts(argument_types)?;
        if let Some(invocation) = invocation {
            self.charge(size_of::<RoutineInvocationBinding>())?;
            let RoutineInvocationBinding {
                argument_positions,
                argument_targets,
                argument_sources,
                parameter_types,
                return_type,
                variadic_mode: _,
            } = invocation.as_ref();
            self.buffer::<usize>(argument_positions.capacity())?;
            self.texts(argument_targets)?;
            self.buffer::<Option<String>>(argument_sources.capacity())?;
            for source in argument_sources {
                self.optional_text(source.as_ref())?;
            }
            self.texts(parameter_types)?;
            self.optional_text(return_type.as_ref())?;
        }
        if let Some(error) = resolution_error {
            match error {
                FunctionResolutionError::UndefinedFunction { signature } => self.text(signature)?,
                FunctionResolutionError::Operator(error) => {
                    self.charge(size_of::<OperatorResolutionError>())?;
                    let OperatorResolutionError { sqlstate, message } = error.as_ref();
                    self.text(sqlstate)?;
                    self.text(message)?;
                }
            }
        }
        Ok(())
    }
}
