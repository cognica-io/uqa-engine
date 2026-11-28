//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed scalar rewrites preserve selected routine context and admitted constant results.

use super::{constants, ColumnType, Context, OwnedScalarExpr, SQLError, ScalarExpr};

impl Context<'_, '_> {
    pub(super) fn finish(
        &self,
        expression: OwnedScalarExpr,
        integer_syntax: bool,
        row_syntax: bool,
        result_type: Option<&ColumnType>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let config = self.production.config;
        let storage = &self.production.storage;
        let control = &config.production;
        if let (
            Some(context),
            ScalarExpr::Func {
                binding: Some(binding),
                args,
                distinct: false,
                order_by,
                filter: None,
                ..
            },
        ) = (config.routine_inlining.as_ref(), &*expression)
        {
            if order_by.is_empty() {
                if let Some(inlined) =
                    context.prepare_owned(binding, args, config.active_inline_routines, control)?
                {
                    return config.with_inline_routine(inlined.identity, |nested| {
                        drop(expression);
                        let nested = Context {
                            production: super::ScalarProduction::admitted(nested),
                        };
                        let expression = nested.scalar(inlined.expression).map_err(|error| {
                            match inlined.error_context {
                                Some(context) => {
                                    let (context, memory) = context.into_parts();
                                    let error = error.with_context(context);
                                    drop(memory);
                                    error
                                }
                                None => error,
                            }
                        })?;
                        let expression =
                            constants::metadata::retain_record_result(expression, false, control)?;
                        let expression = constants::metadata::retain_computed_integer(
                            expression, None, control,
                        )?;
                        self.production.adopt(expression)
                    });
                }
            }
        }
        let types = config
            .routine_inlining
            .as_ref()
            .map(|context| context.types);
        let resolver = config
            .routine_inlining
            .as_ref()
            .map(|context| context.routines as &dyn uqa_sql::FunctionTypeResolver);
        constants::check_inlined_similar_substring(
            &expression,
            config.builtin_permissions.as_deref(),
            types,
            resolver,
            control,
        )?;
        let expression =
            constants::metadata::retain_record_result(expression, row_syntax, storage)?;
        let expression =
            match constants::fold_owned_composite_constructor(&expression, types, control)? {
                Some(replacement) => self.production.adopt(replacement)?,
                None => expression,
            };
        let expression = constants::owned::fold(expression, result_type, self.production)?;
        let expression = if integer_syntax {
            expression
        } else {
            constants::metadata::retain_computed_integer(expression, None, storage)?
        };
        if config.retain_values_constants {
            constants::owned::retain_large_literal(expression, self.production)
        } else {
            Ok(expression)
        }
    }
}
