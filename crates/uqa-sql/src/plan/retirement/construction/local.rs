//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One active field owner retires a native node before the existing tree visits its children.

use super::{Node, ScalarRetirement};
use crate::plan::retirement::layouts::Layouts;

pub(super) trait NodeRetirement: Default {
    fn advance(&mut self, node: &mut Node) -> bool;
}

impl NodeRetirement for ScalarRetirement {
    fn advance(&mut self, node: &mut Node) -> bool {
        ScalarRetirement::advance(self, node)
    }
}

#[derive(Default)]
pub(super) struct Local {
    scalars: ScalarRetirement,
    layouts: Option<Layouts>,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local")
            .field("scalars", &self.scalars)
            .field("layouts", &self.layouts.is_some())
            .finish()
    }
}

impl NodeRetirement for Local {
    fn advance(&mut self, node: &mut Node) -> bool {
        if let Some(layouts) = &mut self.layouts {
            if layouts.advance() {
                self.layouts = None;
                return true;
            }
            return false;
        }
        if !self.scalars.advance(node) {
            return false;
        }
        match node {
            Node::Query(query)
                if query.analyzed_output.is_some() || query.analyzed_source.is_some() =>
            {
                self.layouts = Some(Layouts::replace(query, None, None));
                false
            }
            _ => true,
        }
    }
}
