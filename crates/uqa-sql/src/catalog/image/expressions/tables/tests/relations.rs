//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{collect, complete};
use crate::{
    ast::{
        ConstraintCatalogIdentity, DetachedPartitionConstraint, Expr, ForeignKey, ForeignKeyAction,
        ForeignKeyMatch, PartitionBound, PartitionIdentityOverride, PartitionRangeDatum,
        PartitionSpec, PartitionStrategy, ReferencedPartitionConstraint, TableCheck,
        TableHierarchy, TableKeyConstraint, TableKeyConstraintKind,
    },
    catalog::{
        image::{self, DefinitionImageReader},
        relation_attributes::DroppedAttribute,
    },
    SQLError,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

mod ownership;

#[derive(Debug, serde::Serialize)]
enum Component {
    Checks(Vec<TableCheck>),
    Keys(Vec<TableKeyConstraint>),
    Foreign(Vec<ForeignKey>),
    Hierarchy(TableHierarchy),
    Dropped(Vec<DroppedAttribute>),
}

impl Component {
    fn reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        token: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        match self {
            Self::Checks(values) => image::table_checks_image_reader(values, memory, token),
            Self::Keys(values) => image::table_keys_image_reader(values, memory, token),
            Self::Foreign(values) => image::foreign_keys_image_reader(values, memory, token),
            Self::Hierarchy(value) => image::table_hierarchy_image_reader(value, memory, token),
            Self::Dropped(values) => image::dropped_attributes_image_reader(values, memory, token),
        }
    }

    fn decode(
        &self,
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        token: &CancellationToken,
    ) -> Result<Budgeted<Self>, SQLError> {
        fn wrap<T>(
            value: Budgeted<T>,
            convert: impl FnOnce(T) -> Component,
        ) -> Budgeted<Component> {
            let (value, memory) = value.into_parts();
            Budgeted::new(convert(value), memory)
        }
        Ok(match self {
            Self::Checks(_) => wrap(
                image::read_table_checks_image(reader, memory, token)?,
                Self::Checks,
            ),
            Self::Keys(_) => wrap(
                image::read_table_keys_image(reader, memory, token)?,
                Self::Keys,
            ),
            Self::Foreign(_) => wrap(
                image::read_foreign_keys_image(reader, memory, token)?,
                Self::Foreign,
            ),
            Self::Hierarchy(_) => wrap(
                image::read_table_hierarchy_image(reader, memory, token)?,
                Self::Hierarchy,
            ),
            Self::Dropped(_) => wrap(
                image::read_dropped_attributes_image(reader, memory, token)?,
                Self::Dropped,
            ),
        })
    }

    fn encode(&self, size: usize) -> Vec<u8> {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let bytes = collect(self.reader(&memory, &token).unwrap(), size);
        assert_eq!(memory.used(), 0);
        bytes
    }

    fn round_trip(&self) {
        let bytes = self.encode(31);
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let saved = self.decode(&mut bytes.as_slice(), &memory, &token).unwrap();
        assert_eq!(
            serde_json::to_value(&*saved).unwrap(),
            serde_json::to_value(self).unwrap()
        );
        assert_eq!(saved.encode(20000), bytes);
        drop(saved);
        assert_eq!(memory.used(), 0);
    }
}

fn check() -> TableCheck {
    TableCheck {
        name: None,
        object_id: None,
        catalog_oid: None,
        is_local: false,
        expr: Expr::Not(Box::new(Expr::Star)),
        enforced: true,
        validated: false,
        no_inherit: true,
        partition_constraint: Some(DetachedPartitionConstraint {
            spec: PartitionSpec {
                strategy: PartitionStrategy::Range,
                keys: vec![Expr::Star],
            },
            bound: PartitionBound::Range {
                lower: vec![PartitionRangeDatum::MinValue],
                upper: vec![PartitionRangeDatum::MaxValue],
            },
        }),
    }
}

fn key() -> TableKeyConstraint {
    TableKeyConstraint {
        catalog_identity: None,
        index_identity: None,
        name: None,
        kind: TableKeyConstraintKind::Unique,
        columns: vec!["k".into()],
        included_columns: vec!["v".into(), "v".into()],
        nulls_not_distinct: true,
        without_overlaps: false,
    }
}

fn foreign() -> ForeignKey {
    ForeignKey {
        referenced_index: None,
        referenced_key: None,
        name: None,
        object_id: None,
        catalog_identity: None,
        local_columns: vec!["k".into()],
        ref_table: "p".into(),
        ref_columns: vec!["v".into()],
        on_update: ForeignKeyAction::Cascade,
        on_delete: ForeignKeyAction::SetNull,
        on_delete_set_columns: vec!["k".into()],
        match_type: ForeignKeyMatch::Full,
        enforced: true,
        validated: false,
        deferrable: true,
        initially_deferred: false,
        period: true,
        referenced_partitions: vec![],
    }
}

fn dropped() -> DroppedAttribute {
    DroppedAttribute {
        number: 3,
        type_length: -1,
        type_modifier: 17,
        dimensions: 2,
        by_value: false,
        alignment: "i".into(),
        storage: "x".into(),
        collation: 950,
        identity: String::new(),
        is_local: true,
        inheritance_count: 2,
    }
}

fn header(magic: [u8; 8]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes
}

fn text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

#[test]
fn native_table_metadata_images_match_independent_wire_records() {
    let mut checked = header(*b"UQATCK01");
    checked.extend([0, 0, 0, 0, 1, 0, 1, 1, 15, 0, 1]);
    checked.extend(1u64.to_le_bytes());
    checked.extend([0, 2]);
    checked.extend(1u64.to_le_bytes());
    checked.extend(1u64.to_le_bytes());
    checked.extend([0, 2]);
    let mut keyed = header(*b"UQATKY01");
    keyed.extend([0, 0, 0, 1, 1, 0]);
    keyed.extend(1u64.to_le_bytes());
    keyed.extend(2u64.to_le_bytes());
    for name in ["k", "v", "v"] {
        text(&mut keyed, name);
    }
    let mut referenced = header(*b"UQAFKY01");
    referenced.extend([0; 5]);
    text(&mut referenced, "p");
    referenced.extend([2, 3, 1, 1, 0, 1, 0, 1]);
    for count in [1u64, 1, 1, 0] {
        referenced.extend(count.to_le_bytes());
    }
    for name in ["k", "v", "k"] {
        text(&mut referenced, name);
    }
    let mut hierarchy = header(*b"UQATHR01");
    hierarchy.extend(1u64.to_le_bytes());
    hierarchy.extend(1u64.to_le_bytes());
    hierarchy.extend([1, 1]);
    for _ in 0..4 {
        hierarchy.extend(0u64.to_le_bytes());
    }
    text(&mut hierarchy, "p");
    hierarchy.extend(7i32.to_le_bytes());
    hierarchy.push(2);
    hierarchy.extend(1u64.to_le_bytes());
    hierarchy.extend([0, 3]);
    hierarchy.extend(8i32.to_le_bytes());
    hierarchy.extend(3i32.to_le_bytes());
    let mut removed = header(*b"UQADAT01");
    removed.extend(3i16.to_le_bytes());
    for value in [-1i64, 17, 2] {
        removed.extend(value.to_le_bytes());
    }
    removed.push(0);
    text(&mut removed, "i");
    text(&mut removed, "x");
    removed.extend(950i64.to_le_bytes());
    text(&mut removed, "");
    removed.push(1);
    removed.extend(2i64.to_le_bytes());
    for (component, expected) in [
        (Component::Checks(vec![check()]), checked),
        (Component::Keys(vec![key()]), keyed),
        (Component::Foreign(vec![foreign()]), referenced),
        (
            Component::Hierarchy(TableHierarchy {
                parents: vec!["p".into()],
                parent_sequence_numbers: vec![7],
                partition_spec: Some(PartitionSpec {
                    strategy: PartitionStrategy::Hash,
                    keys: vec![Expr::Star],
                }),
                partition_bound: Some(PartitionBound::Hash {
                    modulus: 8,
                    remainder: 3,
                }),
                ..TableHierarchy::default()
            }),
            hierarchy,
        ),
        (Component::Dropped(vec![dropped()]), removed),
    ] {
        for size in [1, 13, 127, 20000] {
            assert_eq!(component.encode(size), expected);
        }
        component.round_trip();
    }
}

fn complete_components() -> Vec<Component> {
    let identity = ConstraintCatalogIdentity {
        object_id: [7; 16],
        oid: 65536,
    };
    let mut checked = check();
    checked.name = Some("original.check".into());
    checked.object_id = Some([8; 16]);
    checked.catalog_oid = Some(65537);
    let mut keyed = key();
    keyed.catalog_identity = Some(identity);
    keyed.index_identity = Some(identity);
    keyed.name = Some("original.key".into());
    let mut referenced = foreign();
    referenced.referenced_index = Some([9; 16]);
    referenced.referenced_key = Some("original.index".into());
    referenced.name = Some("original.fk".into());
    referenced.object_id = Some([10; 16]);
    referenced.catalog_identity = Some(identity);
    referenced.referenced_partitions = vec![ReferencedPartitionConstraint {
        partition: [11; 16],
        parent: Some([12; 16]),
        name: "child.fk".into(),
        catalog_identity: identity,
        validated: false,
    }];
    let hierarchy = TableHierarchy {
        parents: vec!["second".into(), "first".into()],
        parent_sequence_numbers: vec![7, 3],
        partition_spec: Some(PartitionSpec {
            strategy: PartitionStrategy::Range,
            keys: vec![Expr::Star, Expr::Default],
        }),
        partition_bound: Some(PartitionBound::Range {
            lower: vec![
                PartitionRangeDatum::MinValue,
                PartitionRangeDatum::Value(Expr::Star),
            ],
            upper: vec![
                PartitionRangeDatum::MaxValue,
                PartitionRangeDatum::Value(Expr::Default),
            ],
        }),
        local_columns: vec!["local".into(), "other".into()],
        partition_identity_overrides: vec![
            PartitionIdentityOverride {
                column: "old".into(),
                original: complete().auto_increment,
            },
            PartitionIdentityOverride {
                column: "empty".into(),
                original: None,
            },
        ],
        partition_inherited_key_constraints: vec![keyed.clone()],
        partition_inherited_foreign_keys: vec![referenced.clone()],
    };
    vec![
        Component::Checks(vec![checked]),
        Component::Keys(vec![keyed]),
        Component::Foreign(vec![referenced]),
        Component::Hierarchy(hierarchy),
        Component::Dropped(vec![dropped()]),
    ]
}

#[test]
fn native_table_metadata_images_preserve_constraints_and_partition_variants() {
    for component in complete_components() {
        component.round_trip();
    }
    for component in [
        Component::Checks(vec![]),
        Component::Keys(vec![]),
        Component::Foreign(vec![]),
        Component::Hierarchy(TableHierarchy::default()),
        Component::Dropped(vec![]),
    ] {
        component.round_trip();
    }
    for strategy in [
        PartitionStrategy::List,
        PartitionStrategy::Range,
        PartitionStrategy::Hash,
    ] {
        for bound in [
            PartitionBound::Default,
            PartitionBound::List(vec![Expr::Star, Expr::Default]),
            PartitionBound::Range {
                lower: vec![],
                upper: vec![],
            },
            PartitionBound::Hash {
                modulus: 9,
                remainder: 8,
            },
        ] {
            Component::Hierarchy(TableHierarchy {
                partition_spec: Some(PartitionSpec {
                    strategy,
                    keys: vec![],
                }),
                partition_bound: Some(bound),
                ..TableHierarchy::default()
            })
            .round_trip();
        }
    }
    for mask in 0u8..32 {
        let mut value = foreign();
        value.enforced = mask & 1 != 0;
        value.validated = mask & 2 != 0;
        value.deferrable = mask & 4 != 0;
        value.initially_deferred = mask & 8 != 0;
        value.period = mask & 16 != 0;
        value.match_type = if mask & 1 == 0 {
            ForeignKeyMatch::Simple
        } else {
            ForeignKeyMatch::Full
        };
        let actions = [
            ForeignKeyAction::NoAction,
            ForeignKeyAction::Restrict,
            ForeignKeyAction::Cascade,
            ForeignKeyAction::SetNull,
            ForeignKeyAction::SetDefault,
        ];
        for action in actions {
            value.on_update = action;
            value.on_delete = action;
            Component::Foreign(vec![value.clone()]).round_trip();
        }
        let mut value = check();
        value.is_local = mask & 1 != 0;
        value.enforced = mask & 2 != 0;
        value.validated = mask & 4 != 0;
        value.no_inherit = mask & 8 != 0;
        if mask & 16 == 0 {
            value.partition_constraint = None;
        }
        Component::Checks(vec![value]).round_trip();
        let mut value = key();
        value.kind = if mask & 1 == 0 {
            TableKeyConstraintKind::PrimaryKey
        } else {
            TableKeyConstraintKind::Unique
        };
        value.nulls_not_distinct = mask & 2 != 0;
        value.without_overlaps = mask & 4 != 0;
        Component::Keys(vec![value]).round_trip();
    }
}
