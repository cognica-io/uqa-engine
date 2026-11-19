//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    super::{Frame, Owner},
    invalid, Fields, Input, Kind, Node, SQLError,
};
use crate::{
    ast::{FunctionParam, Statement},
    catalog::work::{ReferenceWork, Work},
    plan::retirement::RetainedTree,
};
use std::{io::Read, ops::Deref};
use uqa_core::{
    image_io::ImageDecoder,
    memory::{Budgeted, MemoryBudget, MemoryError, ProductionControl},
    CancellationToken,
};

/// Immutable original stored syntax with admitted construction and iterative retirement.
#[derive(Debug)]
pub struct CapturedStatement {
    owner: Owner<Node>,
}

impl Deref for CapturedStatement {
    type Target = Statement;
    fn deref(&self) -> &Self::Target {
        match &self.owner.tree.root {
            Node::Statement(value) => value,
            _ => unreachable!("immutable original statement owner"),
        }
    }
}

impl Statement {
    /// Resume the original native syntax across independent byte and continuation-poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<CapturedStatement, SQLError>, SQLError> {
        statement_input(memory, cancellation)
    }

    /// Reconstruct a stored query, mutation or NOTIFY body without parsing, lowering, binding or executing it.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<CapturedStatement, SQLError> {
        let mut input = Input::new(reader, memory, cancellation);
        if input.header(*b"UQASTM01")? != 1 {
            return Err(invalid("stored statement root count"));
        }
        let owner = input.native_tree::<Node>(Kind::Statement)?;
        input.binary.end()?;
        Ok(CapturedStatement { owner })
    }
}

fn statement_input(
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ImageDecoder<CapturedStatement, SQLError>, SQLError> {
    Ok(ImageDecoder::new(
        memory,
        cancellation,
        |mut reader| async move {
            let mut retained_children = 0;
            let mut input = Fields {
                binary: reader.input(),
                retained_children: &mut retained_children,
            };
            if input.header(*b"UQASTM01").await? != 1 {
                return Err(invalid("stored statement root count"));
            }
            let owner = await_field!(input.native_tree::<Node>(Kind::Statement))?;
            input.binary.end().await?;
            Ok(CapturedStatement { owner })
        },
    )?)
}

/// A copied field admits its original syntax depth before the binding owner can edit or retire it.
pub(crate) struct StoredSyntaxRetirement(Budgeted<Box<RetainedTree<Frame<Node>>>>);

impl StoredSyntaxRetirement {
    pub(crate) fn prepare(
        slot: &mut Option<Self>,
        depth: usize,
        memory: &MemoryBudget,
    ) -> Result<(), SQLError> {
        if depth == 0 {
            return Ok(());
        }
        if slot.is_none() {
            let lease = memory.reserve(size_of::<RetainedTree<Frame<Node>>>())?;
            *slot = Some(Self(Budgeted::new(
                Box::new(RetainedTree::new(memory)),
                lease,
            )));
        }
        slot.as_mut()
            .expect("selected syntax workspace")
            .0
            .as_mut()
            .frames
            .reserve(depth.checked_add(1).ok_or(MemoryError::SizeOverflow)?)?;
        Ok(())
    }

    pub(crate) fn statement(&mut self, value: Statement) {
        self.0.as_mut().root = Node::Statement(value);
    }

    pub(crate) fn parameter(&mut self, value: FunctionParam) {
        self.0.as_mut().root = Node::RoutineParam(value);
    }

    pub(crate) async fn finish(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
        captured_depth: bool,
    ) -> Result<(), SQLError> {
        let mut input = ReferenceWork { control, work };
        loop {
            let tree = self.0.as_mut();
            let done = if captured_depth {
                tree.advance()
            } else {
                tree.advance_with_extra(|frames, child| {
                    // A discarded edit can introduce syntax beyond the captured field's depth.
                    // Its ordinary owning destructor remains available under an exhausted allowance.
                    let _ = frames.try_push(Frame::new(child));
                })
            };
            if done {
                return Ok(());
            }
            input.checkpoint().await?;
        }
    }
}

impl Drop for StoredSyntaxRetirement {
    fn drop(&mut self) {
        // Unwind uses only existing capacity, including for an uncommitted externally replaced field.
        while !self.0.as_mut().advance_with_extra(|_, _| ()) {}
    }
}
