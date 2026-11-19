//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently typed callbacks let native input suspend with the retained catalog owner while ordinary callers keep their original closures.

use super::{Expr, FromClause, SQLError, StoredAstVisitor, Work};
use crate::ast::{FunctionBinding, MergeStmt, Projection};
use std::future::{ready, Future};
use uqa_core::memory::ProductionControl;

pub(super) trait Actions<W> {
    fn source(&mut self, _source: &mut FromClause) -> Result<(), SQLError> {
        Ok(())
    }
    fn merge(&mut self, _merge: &mut MergeStmt) -> Result<(), SQLError> {
        Ok(())
    }
    fn projection(&mut self, _projection: &mut Projection) -> Result<(), SQLError> {
        Ok(())
    }
    fn ty(&mut self, _ty: &mut String) {}
    fn relation(&mut self, _name: &mut String) -> Result<(), SQLError> {
        Ok(())
    }

    fn expression(
        &mut self,
        value: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
    fn routine(
        &mut self,
        name: &mut String,
        binding: Option<&mut Option<FunctionBinding>>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

impl<W, R, F> Actions<W> for StoredAstVisitor<'_, R, F>
where
    R: FnMut(&mut String) -> Result<(), SQLError>,
    F: FnMut(&mut String, Option<&mut Option<FunctionBinding>>) -> Result<(), SQLError>,
{
    fn source(&mut self, source: &mut FromClause) -> Result<(), SQLError> {
        self.source.as_mut().map_or(Ok(()), |visit| visit(source))
    }
    fn merge(&mut self, merge: &mut MergeStmt) -> Result<(), SQLError> {
        self.merge.as_mut().map_or(Ok(()), |visit| visit(merge))
    }
    fn projection(&mut self, projection: &mut Projection) -> Result<(), SQLError> {
        self.projection
            .as_mut()
            .map_or(Ok(()), |visit| visit(projection))
    }
    fn ty(&mut self, ty: &mut String) {
        if let Some(visit) = self.ty.as_mut() {
            visit(ty);
        }
    }
    fn relation(&mut self, name: &mut String) -> Result<(), SQLError> {
        (self.relation)(name)
    }
    fn expression(
        &mut self,
        value: &mut Expr,
        _control: &ProductionControl<'_>,
        _work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        ready(
            self.expression
                .as_mut()
                .map_or(Ok(()), |visit| visit(value)),
        )
    }
    fn routine(
        &mut self,
        name: &mut String,
        binding: Option<&mut Option<FunctionBinding>>,
        _control: &ProductionControl<'_>,
        _work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send {
        ready((self.routine)(name, binding))
    }
}

pub(super) struct Reader<'a, 'c, A, W> {
    pub(super) actions: &'a mut A,
    pub(super) control: &'a ProductionControl<'c>,
    pub(super) work: &'a mut W,
}

impl<'a, 'c, A, W: Work + Send> Reader<'a, 'c, A, W> {
    pub(super) fn new(
        actions: &'a mut A,
        control: &'a ProductionControl<'c>,
        work: &'a mut W,
    ) -> Self {
        Self {
            actions,
            control,
            work,
        }
    }
    pub(super) async fn checkpoint(&mut self) -> Result<(), SQLError> {
        crate::catalog::work::ReferenceWork {
            control: self.control,
            work: self.work,
        }
        .checkpoint()
        .await
    }
}
