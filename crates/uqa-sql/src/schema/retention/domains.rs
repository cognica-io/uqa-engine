//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog domain handoff retains constraint expressions before input can invoke callbacks.

use super::{MemoryReservation, Node, Produced, ProductionControl, Result, Walker};
use crate::catalog::domain::StoredDomain;

impl StoredDomain {
    pub(crate) fn retain_external_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        struct Handoff {
            value: StoredDomain,
            memory: Option<MemoryReservation>,
        }
        let mut output = Handoff {
            value: self,
            memory: control.empty_reservation(),
        };
        control.check()?;
        if let Some(budget) = control.budget() {
            let mut walker = Walker::with_control(budget, *control);
            let result = walker
                .visit(Node::Domain(&output.value))
                .and_then(|()| walker.drain());
            let Walker {
                memory, pending, ..
            } = walker;
            drop(pending);
            output
                .memory
                .as_mut()
                .expect("controlled domain handoff")
                .absorb(memory);
            result?;
        }
        Ok(control.finish(output.value, output.memory)?)
    }
}

impl<'a> Walker<'a> {
    pub(super) fn domain(&mut self, domain: &'a StoredDomain) -> Result<()> {
        self.text(&domain.identity.schema)?;
        self.text(&domain.identity.name)?;
        self.optional_text(domain.array_name.as_ref())?;
        if let Some(acl) = &domain.usage_acl {
            self.buffer::<crate::ast::ObjectAclEntry>(acl.capacity())?;
        }
        let definition = &domain.definition;
        self.text(&definition.name)?;
        self.node(Node::Type(&definition.base))?;
        self.optional_text(definition.collation.as_ref())?;
        self.optional_expr(definition.default.as_ref())?;
        if let Some(not_null) = &definition.not_null {
            self.optional_text(not_null.name.as_ref())?;
        }
        self.buffer::<crate::ast::DomainCheck>(definition.checks.capacity())?;
        for check in &definition.checks {
            self.optional_text(check.name.as_ref())?;
            self.node(Node::Expr(&check.expression))?;
        }
        Ok(())
    }
}
