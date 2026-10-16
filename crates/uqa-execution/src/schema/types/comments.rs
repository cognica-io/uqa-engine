//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type descriptions follow object identity through shell completion and type lifecycle changes.

use super::{lifecycle::lock_and_resolve, TypeLifecycleContext};
use uqa_sql::{
    ast::TypeObjectKind,
    catalog::dependencies::{ObjectAddress, TYPE_CLASS},
    SQLError,
};

pub fn comment(
    context: &TypeLifecycleContext<'_>,
    kind: TypeObjectKind,
    name: &str,
    description: Option<&str>,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    let resolved = lock_and_resolve(context, name)?;
    resolved.require_comment_kind(kind, name)?;
    resolved.require_owner(&context.binding)?;
    crate::catalog::comments::set(
        context.comments,
        ObjectAddress::whole(TYPE_CLASS, resolved.oid()),
        description,
    )?;
    context.changes.catalog_registry_changed();
    Ok(())
}
