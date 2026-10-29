//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ObjectAclEntry, Statement},
    catalog::roles::{RoleIdentity, RoleReference},
    routines::registration::RoutineSupportAuthority,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Authority {
    visits: Cell<usize>,
    allowed: Cell<bool>,
    fail: Cell<bool>,
}
impl RoutineSupportAuthority for Authority {
    fn current_user_is_superuser(&self) -> bool {
        false
    }
}
impl RoutineExecutionAuthority for Authority {
    fn current_role(&self) -> RoleReference {
        panic!("identity observation requires no role-name copy")
    }
    fn current_user_has_role_identity_privileges(&self, _: RoleIdentity) -> bool {
        panic!("controlled privilege observation must keep the producer callback")
    }
    fn current_user_has_role_identity_privileges_with_control(
        &self,
        _: RoleIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.visits.set(self.visits.get() + 1);
        if self.fail.get() {
            Err(SQLError::Unsupported("injected membership failure".into()))
        } else {
            Ok(self.allowed.get())
        }
    }
}
fn definition() -> CreateFunction {
    let Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION public.\"a.b\"() RETURNS integer LANGUAGE sql AS $$SELECT 1$$",
    )
    .unwrap()
    .remove(0) else {
        panic!("function declaration");
    };
    definition.owner = Some(RoleIdentity::BOOTSTRAP);
    definition.execute_acl = Some(vec![ObjectAclEntry {
        role: Some(RoleIdentity::BOOTSTRAP),
        grantor: RoleIdentity::BOOTSTRAP,
        grant_option: false,
    }]);
    *definition
}
fn authority() -> Authority {
    Authority {
        visits: Cell::new(0),
        allowed: Cell::new(false),
        fail: Cell::new(false),
    }
}

#[test]
fn routine_privilege_preserves_acl_order_and_propagates_membership_failures() {
    let mut definition = definition();
    let authority = authority();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let error = ensure_routine_execute_privilege_with_control(&authority, &definition, &control)
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine {sqlstate, message} if sqlstate == "42501" && message == "permission denied for function a.b")
    );
    assert_eq!(authority.visits.get(), 1);
    assert_eq!(budget.used(), 0);
    definition
        .execute_acl
        .as_mut()
        .unwrap()
        .push(ObjectAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: false,
        });
    ensure_routine_execute_privilege_with_control(&authority, &definition, &control).unwrap();
    assert_eq!(authority.visits.get(), 2);
    assert_eq!(budget.used(), 0);
    authority.fail.set(true);
    assert!(
        matches!(ensure_routine_execute_privilege_with_control(&authority, &definition, &control), Err(SQLError::Unsupported(message)) if message == "injected membership failure")
    );
    assert_eq!(authority.visits.get(), 3);
    assert_eq!(budget.used(), 0);
}

#[test]
fn routine_name_and_cancellation_admission_precede_privilege_callbacks() {
    let mut definition = definition();
    definition.name = format!("public.{}", "large_name".repeat(4096));
    let authority = authority();
    let budget = MemoryBudget::new(64);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        ensure_routine_execute_privilege_with_control(&authority, &definition, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(authority.visits.get(), 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                ensure_routine_execute_privilege_with_control(&authority, &definition, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(authority.visits.get(), 0);
        assert_eq!(budget.used(), 0);
    }
}
