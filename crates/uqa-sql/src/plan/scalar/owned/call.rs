//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call metadata replacement transfers children while old constructor storage remains owned.

use super::{
    generated::{call_fields, children_from, finish, owned_lowering},
    OwnedScalarExpr, Result, ScalarExpr,
};
use crate::{
    ast::FunctionBinding, plan::scalar::resources::input::Work,
    schema::retention::CatalogRetentionError, ScalarOrder,
};
use uqa_core::memory::{Produced, ProductionControl};

impl OwnedScalarExpr {
    /// Canonicalize a grouping identity's built-in spelling in its existing string buffer. Removing a prefix changes length, never retained capacity.
    pub(crate) fn canonicalize_call_name(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let (ScalarExpr::Func { name, .. } | ScalarExpr::WindowCall { name, .. }) =
            &mut self.expression
        else {
            panic!("call spelling requires a function or window call")
        };
        let mut start = 0;
        while start < name.len() {
            control.check()?;
            let mut end = (start + 4096).min(name.len());
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name[start..end].make_ascii_lowercase();
            start = end;
        }
        if name.starts_with("pg_catalog.") {
            name.drain(.."pg_catalog.".len());
        }
        control.check()?;
        Ok(self)
    }

    /// Preserve the parser's inline ordering-syntax identity without rebuilding any call storage.
    pub fn set_call_order_syntax(
        mut self,
        syntax: crate::ast::FunctionOrderSyntax,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func { order_syntax, .. } = &mut self.expression else {
            panic!("ordering syntax requires a function call")
        };
        *order_syntax = syntax;
        Ok(self)
    }

    /// Change an inline call dispatch without moving or replacing any allocating field.
    pub fn set_call_dispatch(
        mut self,
        dispatch: Option<crate::ast::FunctionDispatch>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut self.expression
        else {
            panic!("dispatch mutation requires a bound function call")
        };
        binding.dispatch = dispatch;
        Ok(self)
    }

    /// Replace a call's selected binding without changing its name, arguments, ordering or filter. The old metadata and containers remain owned until the replacement's admitted containers hold all surviving children.
    pub fn replace_call_binding(
        self,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(self.replace_call_binding_from(binding, control, &mut ()))
    }

    pub(crate) async fn replace_call_binding_from<W: Work>(
        self,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        let ScalarExpr::Func { name, .. } = &self.expression else {
            panic!("binding replacement requires a function call");
        };
        let name = work.text(name, control).await?;
        self.replace_call_metadata_from(name, binding, control, work)
            .await
    }

    /// Install an independently produced call name and binding. This storage operation retains scalar payload identity and performs no overload selection or expression evaluation.
    pub fn replace_call_metadata(
        self,
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(self.replace_call_metadata_from(
            name,
            binding,
            control,
            &mut (),
        ))
    }

    pub(crate) async fn replace_call_metadata_from<W: Work>(
        mut self,
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func {
            args,
            order_by,
            filter,
            distinct,
            order_syntax,
            ..
        } = &self.expression
        else {
            panic!("metadata replacement requires a function call");
        };
        let (argument_count, order_count, has_filter, distinct, order_syntax) = (
            args.len(),
            order_by.len(),
            filter.is_some(),
            *distinct,
            *order_syntax,
        );
        let mut lowering = owned_lowering(control);
        let mut output = call_fields(&mut lowering, name, binding);
        let arguments = children_from(
            &mut lowering,
            (0..argument_count).map(|index| self.take_child(index)),
            work,
        )
        .await?;
        let ScalarExpr::Func {
            args,
            order_by: orders,
            filter,
            distinct: output_distinct,
            order_syntax: output_syntax,
            ..
        } = &mut *output
        else {
            unreachable!()
        };
        *output_distinct = distinct;
        *output_syntax = order_syntax;
        *args = arguments.take();
        lowering.checkpoint(work).await?;
        *orders = lowering.vector(order_count)?;
        for index in 0..order_count {
            lowering.checkpoint(work).await?;
            let ScalarExpr::Func { order_by, .. } = &self.expression else {
                unreachable!();
            };
            let (descending, nulls) = (order_by[index].descending, order_by[index].nulls);
            orders.push(ScalarOrder {
                expr: Self::attach_child(&mut lowering, self.take_child(argument_count + index))?,
                descending,
                nulls,
            });
        }
        if has_filter {
            lowering.checkpoint(work).await?;
            lowering.admit_box::<ScalarExpr>()?;
            *filter = Some(Box::new(Self::attach_child(
                &mut lowering,
                self.take_child(argument_count + order_count),
            )?));
        }
        let output = finish(output.take(), lowering, control)?;
        self.retire_from(control, work).await?;
        control.check()?;
        Ok(output)
    }
}
