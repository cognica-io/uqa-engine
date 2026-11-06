//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::ObjectAclEntry,
    catalog::{
        enum_type::{StoredEnum, StoredEnumLabel},
        foreign_server::{ForeignServerDefinition, ForeignServerMetadata},
        foreign_wrapper::NativeForeignWrapper,
        graph_oids::{EndpointIndexOids, GraphCatalogOids, IndexConstraintOids, LabelCatalogOids},
        named_definitions::{CapturedNamedDefinitions, NamedDefinitions, StoredDefinition},
        relation_oids::RelationCatalogOids,
        shell_type::StoredShellType,
    },
};
use uqa_core::{memory::Produced, EnumLabelKey, RelationIdentity};

mod retention;
mod structured_types;
mod validation;
mod wire;

trait ImageDefinition: StoredDefinition {
    fn reader<'a>(
        saved: &'a CapturedNamedDefinitions<Self>,
        token: &'a CancellationToken,
    ) -> CatalogImageReader<'a>;
    fn decode(
        bytes: &mut dyn Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<NamedDefinitions<Self>, SQLError>;
}
macro_rules! image_definition {
    ($($ty:ty),+ $(,)?) => {$ (
        impl ImageDefinition for $ty {
            fn reader<'a>(saved: &'a CapturedNamedDefinitions<Self>, token: &'a CancellationToken) -> CatalogImageReader<'a> {
                saved.image_reader(token).unwrap()
            }
            fn decode(bytes: &mut dyn Read, memory: &MemoryBudget, token: &CancellationToken) -> Result<NamedDefinitions<Self>, SQLError> {
                NamedDefinitions::<Self>::read_image(bytes, memory, token)
            }
        }
    )+};
}
image_definition!(
    StoredEnum,
    StoredShellType,
    ForeignServerDefinition,
    GraphCatalogOids,
    crate::catalog::composite_type::StoredComposite,
    crate::catalog::range_type::StoredRange
);

fn capture<T: StoredDefinition>(
    values: &NamedDefinitions<T>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Produced<CapturedNamedDefinitions<T>> {
    values
        .capture(&ProductionControl::new(memory, token, token))
        .unwrap()
}
fn encode<T: ImageDefinition>(values: &NamedDefinitions<T>) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let saved = capture(values, &memory, &token);
    streamed(T::reader(&saved, &token), 4096)
}
fn singleton<T: ImageDefinition>(key: &str, value: T) -> NamedDefinitions<T> {
    NamedDefinitions::try_from_iter([(key.into(), value)]).unwrap()
}
fn assert_round_trip<T: ImageDefinition>(values: &NamedDefinitions<T>, expected: &[u8]) {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let saved = capture(values, &memory, &token);
    for width in [1, 2, 7, 63, 4096, 20000] {
        assert_eq!(streamed(T::reader(&saved, &token), width), expected);
    }
    let restored = T::decode(&mut &*expected, &memory, &token).unwrap();
    assert_eq!(encode(&restored), expected);
    let mut written = vec![];
    assert_eq!(
        T::reader(&saved, &token).write_to(&mut written).unwrap(),
        expected.len() as u64
    );
    assert_eq!(written, expected);
    drop((saved, restored));
    assert_eq!(memory.used(), 0);
}

fn enum_value() -> StoredEnum {
    StoredEnum {
        object_id: [7; 16],
        oid: 65536,
        array_oid: 65537,
        array_name: "_e".into(),
        identity: RelationIdentity::new("public", "e"),
        owner: RoleIdentity::BOOTSTRAP,
        labels: vec![
            StoredEnumLabel {
                oid: 65538,
                key: EnumLabelKey::from_bytes(vec![64]).unwrap(),
                label: String::new(),
                sort_order: -0.0,
            },
            StoredEnumLabel {
                oid: 65539,
                key: EnumLabelKey::from_bytes(vec![128]).unwrap(),
                label: "é".into(),
                sort_order: 1.5,
            },
        ],
        usage_acl: None,
    }
}
fn shell_value() -> StoredShellType {
    StoredShellType {
        object_id: [8; 16],
        oid: 65540,
        identity: RelationIdentity::new("public", "s"),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: Some(vec![]),
    }
}
fn server_value() -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: "s".into(),
        fdw_type: "memory_fdw".into(),
        options: [("a".into(), "α".into()), ("z".into(), String::new())]
            .into_iter()
            .collect(),
        metadata: ForeignServerMetadata {
            oid: 65541,
            object_id: [9; 16],
            owner: RoleIdentity::BOOTSTRAP,
            server_type: Some(String::new()),
            version: None,
            option_order: Some(vec!["z".into(), "a".into()]),
            wrapper_reference: Some(NativeForeignWrapper::Memory.reference()),
        },
    }
}
fn graph_value() -> GraphCatalogOids {
    GraphCatalogOids {
        namespace: 65542,
        label_sequence: 65543,
        labels: [(
            17,
            LabelCatalogOids {
                sequence: 65544,
                relation: RelationCatalogOids {
                    relation: 65545,
                    row_type: Some(65546),
                    array_type: Some(65547),
                    rule: Some(65548),
                },
                id_default: 65549,
                properties_default: 65550,
                not_null: [("id".into(), 65551), ("properties".into(), 65552)]
                    .into_iter()
                    .collect(),
                toast_table: 65553,
                toast_index: 65554,
                primary_key: Some(IndexConstraintOids {
                    index: 65555,
                    constraint: 65556,
                }),
                endpoint_indexes: Some(EndpointIndexOids {
                    start_id: 65557,
                    end_id: 65558,
                }),
                trigger: 65559,
            },
        )]
        .into_iter()
        .collect(),
    }
}

fn acl() -> Vec<ObjectAclEntry> {
    let public = ObjectAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        grant_option: false,
    };
    vec![
        public.clone(),
        ObjectAclEntry {
            role: Some(role("alpha", 42).identity()),
            grantor: role("omega", 99).identity(),
            grant_option: true,
        },
        public,
    ]
}

#[test]
fn native_metadata_images_preserve_options_acl_paths_and_legacy_graph_companions() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for usage_acl in [None, Some(vec![]), Some(acl())] {
        let enums = singleton(
            "public.e",
            StoredEnum {
                usage_acl: usage_acl.clone(),
                ..enum_value()
            },
        );
        let shells = singleton(
            "public.s",
            StoredShellType {
                usage_acl: usage_acl.clone(),
                ..shell_value()
            },
        );
        for width in [1, 4096] {
            let saved = capture(&enums, &memory, &token);
            let bytes = streamed(saved.image_reader(&token).unwrap(), width);
            let restored = StoredEnum::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
            let value = &restored["public.e"];
            assert_eq!(value.labels, enum_value().labels);
            assert_eq!(value.labels[0].sort_order.to_bits(), 0x8000_0000);
            assert_eq!(value.usage_acl, usage_acl);
        }
        let restored =
            StoredShellType::decode(&mut encode(&shells).as_slice(), &memory, &token).unwrap();
        assert_eq!(restored["public.s"].usage_acl, usage_acl);
    }
    let mut graph = graph_value();
    let label = graph.labels.get_mut(&17).unwrap();
    label.relation.row_type = None;
    label.relation.array_type = None;
    label.relation.rule = None;
    label.primary_key = None;
    label.endpoint_indexes = None;
    let graphs = singleton("Exact.Graph", graph);
    let bytes = encode(&graphs);
    let restored = GraphCatalogOids::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(restored == graphs);
    drop(restored);
    for absent in [true, false] {
        let mut server = server_value();
        server.metadata.server_type = None;
        server.metadata.version = Some("0.0 unchanged".into());
        if absent {
            server.metadata.option_order = None;
            server.metadata.wrapper_reference = None;
        }
        let servers = singleton("s", server);
        let restored =
            ForeignServerDefinition::decode(&mut encode(&servers).as_slice(), &memory, &token)
                .unwrap();
        assert!(restored == servers);
    }
    assert_eq!(memory.used(), 0);
}
