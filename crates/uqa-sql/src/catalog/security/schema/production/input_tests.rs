//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleBinding, RoleDefinition, RoleMembership};
use std::{cell::Cell, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

#[derive(Clone)]
struct Fixture {
    roles: RoleDefinitions,
    memberships: RoleMemberships,
    security: BoundSchemaSecurity,
}
impl Fixture {
    fn new() -> Self {
        let mut roles = RoleDefinitions::new();
        roles
            .insert("uqa".into(), RoleDefinition::bootstrap())
            .unwrap();
        for id in 1..=4_u8 {
            let mut role = RoleDefinition::bootstrap();
            role.name = format!("r{id}");
            role.oid = i64::from(id) + 100;
            role.object_id = [id; 16];
            role.attributes.clear();
            roles.insert(role.name.clone(), role).unwrap();
        }
        let mut memberships = RoleMemberships::new();
        for (id, member, role) in [(1, "r1", "r2"), (2, "r2", "r3"), (3, "r3", "r1")] {
            let edge = RoleMembership {
                oid: id,
                member: RoleBinding::from_definition(&roles[member]).unwrap(),
                role: RoleBinding::from_definition(&roles[role]).unwrap(),
                grantor: RoleBinding::from_definition(&roles["uqa"]).unwrap(),
                admin_option: false,
                inherit_option: true,
                set_option: false,
            };
            memberships.insert(edge.key(), edge).unwrap();
        }
        let security = BoundSchemaSecurity {
            tuple: None,
            role_owner: roles["r3"].identity(),
            acl: None,
        };
        Self {
            roles,
            memberships,
            security,
        }
    }
    fn expected(
        &self,
        subject: &RoleReference,
        privilege: SchemaAclPrivilege,
    ) -> Result<bool, String> {
        self.security.resolve(&self.roles).map(|security| {
            super::super::role_has_schema_privilege(
                &security,
                subject,
                privilege,
                &self.roles,
                &self.memberships,
            )
        })
    }
}
type Decoder = ImageDecoder<bool, SQLError>;
fn decoder(
    fixture: Fixture,
    subject: RoleReference,
    privilege: SchemaAclPrivilege,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        role_has_schema_privilege_from_input(
            (&fixture.security).into(),
            &subject,
            privilege,
            &fixture.roles,
            &fixture.memberships,
            &ProductionControl::new(&native, &original, input.cancellation),
            &mut input,
        )
        .await
    })
    .unwrap()
}
fn advance(input: &mut Decoder, steps: usize) -> Result<Option<bool>, SQLError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}
fn complete(input: &mut Decoder, steps: usize) -> Result<(bool, usize), SQLError> {
    for polls in 1..100_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, polls));
        }
    }
    panic!("schema privilege input stopped progressing")
}

#[test]
fn retained_schema_privileges_preserve_native_roles_inheritance_cycles_and_acl_error_order() {
    let initial = Fixture::new();
    let bound = RoleReference::Bound(Arc::new(
        RoleBinding::from_definition(&initial.roles["r1"]).unwrap(),
    ));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for variant in 0..8 {
        let mut fixture = initial.clone();
        match variant {
            1 => fixture.security = BoundSchemaSecurity::with_public_privileges(true),
            2 => {
                fixture.security = BoundSchemaSecurity::with_public_privileges(true);
                fixture
                    .security
                    .acl
                    .as_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap()
                    .grantor = fixture.roles["r4"].identity();
                fixture.roles.remove("r4").unwrap();
            }
            3 => {
                let mut role = fixture.roles["r1"].clone();
                fixture.roles.remove("r1").unwrap();
                role.name = "renamed".into();
                fixture.roles.insert(role.name.clone(), role).unwrap();
                let mut replacement = initial.roles["r4"].clone();
                replacement.name = "r1".into();
                fixture.roles.insert("r1".into(), replacement).unwrap();
            }
            4 => {
                fixture.roles.remove("r1").unwrap();
            }
            5 => {
                let key = *fixture
                    .memberships
                    .iter()
                    .find(|(_, edge)| edge.oid == 2)
                    .unwrap()
                    .0;
                fixture
                    .memberships
                    .edit(&key, |edge| {
                        edge.inherit_option = false;
                        Ok(())
                    })
                    .unwrap();
            }
            6 => fixture.security.role_owner.object_id = [0; 16],
            7 => fixture.security.role_owner = fixture.roles["r4"].identity(),
            _ => {}
        }
        for subject in [
            RoleReference::from("r1"),
            "r4".into(),
            "uqa".into(),
            "missing".into(),
            bound.clone(),
        ] {
            for privilege in [SchemaAclPrivilege::Usage, SchemaAclPrivilege::Create] {
                let expected = fixture.expected(&subject, privilege);
                for steps in [1, 7, 128] {
                    let actual = complete(
                        &mut decoder(
                            fixture.clone(),
                            subject.clone(),
                            privilege,
                            &native,
                            &work,
                            &token,
                            &token,
                        ),
                        steps,
                    )
                    .map(|(value, _)| value)
                    .map_err(|error| match error {
                        SQLError::Internal(message) => message,
                        other => panic!("{other}"),
                    });
                    assert_eq!(actual, expected, "variant {variant}, {subject:?}");
                    assert_eq!((native.used(), work.used()), (0, 0));
                }
            }
        }
    }
}

#[test]
fn retained_schema_privileges_release_traversal_at_each_pending_boundary() {
    let fixture = Fixture::new();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let subject = RoleReference::from("r1");
    let polls = complete(
        &mut decoder(
            fixture.clone(),
            subject.clone(),
            SchemaAclPrivilege::Usage,
            &native,
            &work,
            &token,
            &token,
        ),
        1,
    )
    .unwrap()
    .1;
    assert!(polls > 10);
    let mut denials = 0;
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop", "memory", "window"] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(
                fixture.clone(),
                subject.clone(),
                SchemaAclPrivilege::Usage,
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            let hold = (action == "memory").then(|| native.reserve(native.available()).unwrap());
            match action {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => {}
            }
            if action != "drop" {
                let result = complete(&mut input, usize::from(action != "window"));
                if action == "memory" {
                    if let Err(error) = &result {
                        assert_eq!(error.sqlstate(), Some("53200"));
                        denials += 1;
                    }
                } else {
                    assert!(result.is_err());
                }
                drop(result);
            }
            drop((input, hold));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
    assert!(denials > 0);
}

#[test]
fn ordinary_schema_privilege_subject_callbacks_keep_their_original_sequence() {
    struct Subject(Cell<usize>);
    impl RoleSubject for Subject {
        fn role_name<'a>(&'a self, _: &'a RoleDefinitions) -> Option<&'a str> {
            Some("r1")
        }
        fn role_definition<'a>(&self, roles: &'a RoleDefinitions) -> Option<&'a RoleDefinition> {
            let index = self.0.get();
            self.0.set(index + 1);
            roles.get(if index == 0 { "r4" } else { "r1" })
        }
    }
    let fixture = Fixture::new();
    let subject = Subject(Cell::new(0));
    assert!(role_has_schema_privilege_with_control(
        (&fixture.security).into(),
        &subject,
        SchemaAclPrivilege::Usage,
        &fixture.roles,
        &fixture.memberships,
        &ProductionControl::uncontrolled(),
    )
    .unwrap());
    assert_eq!(subject.0.get(), 2);
}
