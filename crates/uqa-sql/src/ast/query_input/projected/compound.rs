//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Flattened declarations observe every unknown field before their selected native constructors.

use super::{
    finish, native, FlatFields, Input, JsonDiagnostic, JsonInputError, JsonPosition, JsonReadError,
    Native, Parts, Read, Result,
};
use crate::{
    ast::{
        CteBody, FunctionBinding, FunctionValueBinding, RoutineSecurityAttributes, Statement, CTE,
    },
    explain::{ExplainOption, ExplainOptions},
};
use uqa_core::memory::Produced;

const CTE_KEYS: &[&str; 5] = &["query", "insert", "update", "delete", "merge"];
projected_struct!(CTE {
    name: required, columns: required, recursive: required,
    materialization: defaulted, search: default(None), cycle: default(None)
} flatten(body, CTE_KEYS, read_cte_body));

async fn read_cte_body<'a>(
    input: Input<'a>,
    fields: &FlatFields<'a, 5>,
    work: &mut super::Work<'a>,
) -> Result<Native<CteBody>> {
    let Some((index, child)) = fields.values(input).next() else {
        return Err(JsonDiagnostic::message(
            "no variant of enum CteBody found in flattened data",
            JsonPosition::unlocated(),
        )
        .into());
    };
    let mut parts = Parts::new(input.control);
    let value = match index {
        0 => CteBody::Query(parts.take(child.read(work).await?)),
        1 => CteBody::Insert(parts.take(child.read(work).await?)),
        2 => CteBody::Update(parts.take(child.read(work).await?)),
        3 => CteBody::Delete(parts.take(child.read(work).await?)),
        4 => CteBody::Merge(parts.take(child.read(work).await?)),
        _ => unreachable!("declared CTE body"),
    };
    finish(parts, value)
}

const BINDING_KEYS: &[&str; 2] = &["composite_field", "assignment"];
projected_struct!(FunctionBinding {
    object_id: default(None), name: required, argument_types: required,
    builtin: defaulted, dispatch: default(None), invocation: default(None),
    resolution_error: default(None), input_collation: default(None)
} flatten(value_binding, BINDING_KEYS, read_value_binding));

async fn read_value_binding<'a>(
    input: Input<'a>,
    fields: &FlatFields<'a, 2>,
    work: &mut super::Work<'a>,
) -> Result<Native<Option<Box<FunctionValueBinding>>>> {
    let Some((index, child)) = fields.values(input).next() else {
        return input.scalar(None);
    };
    let value = async {
        let mut parts = Parts::new(input.control);
        let value = match index {
            0 => FunctionValueBinding::CompositeField(parts.take(child.read(work).await?)),
            1 => FunctionValueBinding::Assignment(parts.take(child.read(work).await?)),
            _ => unreachable!("declared value binding"),
        };
        finish(parts, value)
    }
    .await;
    let value = match value {
        Ok(value) => value,
        // Serde's optional flattened enum discards the selected variant's data error.
        Err(JsonInputError::Diagnostic(_)) => return input.scalar(None),
        Err(error) => return Err(error),
    };
    let header = input
        .control
        .reserve(size_of::<FunctionValueBinding>())
        .map_err(JsonReadError::from)?;
    let (value, memory) = value.into_parts();
    native(
        input
            .control
            .finish(Some(Box::new(value)), input.control.combine(memory, header))
            .map_err(JsonReadError::from)?,
        input.control,
    )
}

pub(in crate::ast::query_input) const SECURITY_KEYS: &[&str; 2] =
    &["security_definer", "leakproof"];

pub(in crate::ast::query_input) async fn read_security<'a>(
    input: Input<'a>,
    fields: &FlatFields<'a, 2>,
    work: &mut super::Work<'a>,
) -> Result<Native<RoutineSecurityAttributes>> {
    let mut security = RoutineSecurityAttributes::default();
    for (index, child) in fields.values(input) {
        let value = child.read::<bool>(work).await?.into_parts().0;
        match index {
            0 => security.security_definer = value,
            1 => security.leakproof = value,
            _ => unreachable!("declared routine security field"),
        }
    }
    input.scalar(security)
}

const EXPLAIN_KEYS: &[&str; 4] = &["options", "analyze", "verbose", "format"];

async fn read_explain_options<'a>(
    input: Input<'a>,
    fields: &FlatFields<'a, 4>,
    work: &mut super::Work<'a>,
) -> Result<Native<ExplainOptions>> {
    let (mut options, mut analyze, mut verbose, mut format) = (None, None, None, None);
    for (index, child) in fields.values(input) {
        match index {
            0 => options = Some(child.read::<Option<Vec<ExplainOption>>>(work).await?),
            1 => analyze = Some(child.read::<Option<bool>>(work).await?),
            2 => verbose = Some(child.read::<Option<bool>>(work).await?),
            3 => format = Some(child.read::<Option<String>>(work).await?),
            _ => unreachable!("declared EXPLAIN option"),
        }
    }
    if let Some(options) = options {
        let (options, memory) = options.into_parts();
        if let Some(options) = options {
            return native(
                input
                    .control
                    .finish(ExplainOptions { options }, memory)
                    .map_err(JsonReadError::from)?,
                input.control,
            );
        }
    }
    let analyze = analyze.and_then(|value| value.into_parts().0);
    let verbose = verbose.and_then(|value| value.into_parts().0);
    let format = match format {
        Some(format) => Produced::from(format),
        None => input
            .control
            .finish(None, input.control.empty_reservation())
            .map_err(JsonReadError::from)?,
    };
    native(
        ExplainOptions::from_legacy_with_control(analyze, verbose, format, input.control)
            .map_err(JsonReadError::from)?,
        input.control,
    )
}

pub(in crate::ast::query_input) fn read_explain<'i, 'a>(
    input: Input<'i>,
    work: &'a mut crate::ast::query_input::projected::Work<'i>,
) -> Result<Read<'a, Statement>> {
    projected_fields!(input, work, Statement::Explain { body: required } flatten(options, EXPLAIN_KEYS, read_explain_options))
}
