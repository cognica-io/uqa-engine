//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partition bounds retain their own typed datums and need no relation lookup.

use super::{atom, invalid, list, Field, Node, Renderer, SQLError};

impl Renderer<'_> {
    pub(super) fn partition_bound(&self, node: &Node) -> Result<String, SQLError> {
        if node.boolean("is_default")? {
            return Ok("DEFAULT".into());
        }
        match atom(node, "strategy")? {
            "h" => Ok(format!(
                "FOR VALUES WITH (modulus {}, remainder {})",
                node.integer("modulus")?,
                node.integer("remainder")?
            )),
            "l" => Ok(format!(
                "FOR VALUES IN ({})",
                list(node, "listdatums")?
                    .iter()
                    .map(|value| self.partition_constant(value))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )),
            "r" => Ok(format!(
                "FOR VALUES FROM ({}) TO ({})",
                self.partition_range(node, "lowerdatums")?,
                self.partition_range(node, "upperdatums")?
            )),
            _ => Err(invalid("unknown partition strategy")),
        }
    }

    fn partition_constant(&self, value: &Field) -> Result<String, SQLError> {
        match value {
            Field::Node(node) if node.kind == "CONST" => self.constant_labeled(node, false),
            _ => Err(invalid("expected a partition constant")),
        }
    }

    fn partition_range(&self, node: &Node, field: &str) -> Result<String, SQLError> {
        list(node, field)?
            .iter()
            .map(|value| match value {
                Field::Node(datum) if datum.kind == "PARTITIONRANGEDATUM" => {
                    match datum.integer("kind")? {
                        -1 => Ok("MINVALUE".into()),
                        0 => self.partition_constant(datum.field("value")?),
                        1 => Ok("MAXVALUE".into()),
                        _ => Err(invalid("unknown partition range datum kind")),
                    }
                }
                _ => Err(invalid("expected a partition range datum")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|values| values.join(", "))
    }
}
