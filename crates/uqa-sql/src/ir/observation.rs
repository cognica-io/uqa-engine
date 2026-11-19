//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar observations retain ordered child positions across work windows. Query and window bodies remain separate observation scopes.

use super::children::Scope;
use crate::{catalog::work::Work, construction::Stack, SQLError, ScalarExpr};
use std::future::Future;
use uqa_core::memory::ProductionControl;

enum Frame<N: Node> {
    Visit(N),
    Children(N::Children),
}

pub(crate) enum Decision {
    Descend,
    Prune,
    Stop,
}

/// Observe all metadata on the selected scalar before inspecting its first child.
pub(crate) trait ScalarObserver {
    fn scalar<W: Work + Send>(
        &mut self,
        value: &ScalarExpr,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

pub(crate) async fn visit_all<W: Work + Send>(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    observer: &mut impl ScalarObserver,
) -> Result<(), SQLError> {
    let mut nodes = Nodes::<_, 32>::new(expression, Scope::Complete, control)?;
    while let Some(node) = nodes.next(control, work).await? {
        observer.scalar(node, work).await?;
        nodes.descend(node)?;
    }
    Ok(())
}

trait Node: Sized {
    type Children;
    fn children(self, scope: Scope) -> Self::Children;
    fn next(children: &mut Self::Children, scope: Scope) -> Option<Self>;
}

impl<'a> Node for &'a ScalarExpr {
    type Children = (&'a ScalarExpr, usize);
    fn children(self, _: Scope) -> Self::Children {
        (self, 0)
    }
    fn next((expression, index): &mut Self::Children, scope: Scope) -> Option<Self> {
        let child = ScalarExpr::children(expression, scope).nth(*index)?;
        *index += 1;
        Some(child)
    }
}

impl<'a> Node for &'a mut ScalarExpr {
    type Children = super::children::ChildrenMut<'a>;
    fn children(self, scope: Scope) -> Self::Children {
        self.children_mut(scope)
    }
    fn next(children: &mut Self::Children, _: Scope) -> Option<Self> {
        children.next()
    }
}

pub(crate) async fn visit<'a, W, O>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    include_call_order: bool,
    observe: &mut O,
) -> Result<bool, SQLError>
where
    W: Work,
    O: FnMut(&'a ScalarExpr) -> Result<Decision, SQLError> + ?Sized,
{
    visit_in(
        expression,
        control,
        work,
        Scope::Observation {
            call_order: include_call_order,
        },
        observe,
    )
    .await
}

pub(crate) async fn visit_in<'a, W, O>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    scope: Scope,
    observe: &mut O,
) -> Result<bool, SQLError>
where
    W: Work,
    O: FnMut(&'a ScalarExpr) -> Result<Decision, SQLError> + ?Sized,
{
    walk::<_, _, _, 32>(expression, scope, control, work, &mut |node| observe(node)).await
}

pub(crate) async fn visit_mut<W, O>(
    expression: &mut ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    observe: &mut O,
) -> Result<bool, SQLError>
where
    W: Work,
    O: FnMut(&mut ScalarExpr) -> Result<Decision, SQLError> + ?Sized,
{
    walk::<_, _, _, 4>(expression, Scope::Complete, control, work, &mut |node| {
        observe(node)
    })
    .await
}

async fn walk<N, W, O, const INLINE: usize>(
    expression: N,
    scope: Scope,
    control: &ProductionControl<'_>,
    work: &mut W,
    observe: &mut O,
) -> Result<bool, SQLError>
where
    N: Node,
    W: Work,
    O: FnMut(&mut N) -> Result<Decision, SQLError> + ?Sized,
{
    let mut nodes = Nodes::<_, INLINE>::new(expression, scope, control)?;
    while let Some(mut node) = nodes.next(control, work).await? {
        match observe(&mut node)? {
            Decision::Descend => nodes.descend(node)?,
            Decision::Prune => {}
            Decision::Stop => return Ok(true),
        }
    }
    Ok(false)
}

struct Nodes<N: Node, const INLINE: usize> {
    frames: Stack<Frame<N>, INLINE>,
    scope: Scope,
}

impl<N: Node, const INLINE: usize> Nodes<N, INLINE> {
    fn new(root: N, scope: Scope, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut frames = Stack::new(*control);
        frames.push(Frame::Visit(root))?;
        Ok(Self { frames, scope })
    }

    fn descend(&mut self, node: N) -> Result<(), SQLError> {
        Ok(self
            .frames
            .push(Frame::Children(node.children(self.scope)))?)
    }

    async fn next(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut impl Work,
    ) -> Result<Option<N>, SQLError> {
        while let Some(frame) = self.frames.pop() {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            match frame {
                Frame::Visit(expression) => return Ok(Some(expression)),
                Frame::Children(mut children) => {
                    if let Some(child) = N::next(&mut children, self.scope) {
                        self.frames.push(Frame::Children(children))?;
                        self.frames.push(Frame::Visit(child))?;
                    }
                }
            }
        }
        Ok(None)
    }
}
