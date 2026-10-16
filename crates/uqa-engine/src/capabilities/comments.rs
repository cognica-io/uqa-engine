//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain description generations in the owning catalog transaction.

use crate::Engine;
use uqa_execution::catalog::comments::{CommentPublication, CommentRegistry};

impl CommentPublication for Engine {
    fn comments(&self) -> Box<dyn std::ops::Deref<Target = CommentRegistry> + '_> {
        Box::new(self.durable.comments.read())
    }
    fn comment_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn publish_comments(&self, registry: CommentRegistry) {
        *self.durable.comments.write() = registry;
    }
}
