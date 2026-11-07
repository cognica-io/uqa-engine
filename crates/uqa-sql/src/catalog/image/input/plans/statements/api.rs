//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{super::Owner, invalid, Input, Kind, Node, SQLError};
use crate::ast::Statement;
use std::{io::Read, ops::Deref};
use uqa_core::{memory::MemoryBudget, CancellationToken};

/// Immutable original stored syntax with admitted construction and iterative retirement.
#[derive(Debug)]
pub struct CapturedStatement {
    owner: Owner<Node>,
}

impl Deref for CapturedStatement {
    type Target = Statement;
    fn deref(&self) -> &Self::Target {
        match &self.owner.root {
            Node::Statement(value) => value,
            _ => unreachable!("immutable original statement owner"),
        }
    }
}

impl Statement {
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
