use super::*;

#[test]
fn parse_simple_match_return() {
    let q = parse_cypher("MATCH (n:Person) RETURN n.name").unwrap();
    assert_eq!(q.clauses.len(), 2);
    match &q.clauses[0] {
        CypherClause::Match(m) => {
            assert!(!m.optional);
            assert_eq!(m.patterns.len(), 1);
            let elements = &m.patterns[0].elements;
            assert_eq!(elements.len(), 1);
            if let PathElement::Node(np) = &elements[0] {
                assert_eq!(np.variable.as_deref(), Some("n"));
                assert_eq!(np.labels, vec!["Person".to_string()]);
            } else {
                panic!("expected node pattern");
            }
        }
        _ => panic!("expected MATCH"),
    }
    match &q.clauses[1] {
        CypherClause::Return(r) => {
            assert_eq!(r.items.len(), 1);
            if let CypherExpr::PropertyAccess(pa) = &r.items[0].expr {
                assert_eq!(pa.variable, "n");
                assert_eq!(pa.keys, vec!["name".to_string()]);
            } else {
                panic!("expected PropertyAccess");
            }
        }
        _ => panic!("expected RETURN"),
    }
}

#[test]
fn parse_match_with_relationship() {
    let q = parse_cypher("MATCH (a)-[r:KNOWS]->(b) RETURN b").unwrap();
    if let CypherClause::Match(m) = &q.clauses[0] {
        let path = &m.patterns[0];
        assert_eq!(path.elements.len(), 3);
        if let PathElement::Rel(rp) = &path.elements[1] {
            assert_eq!(rp.variable.as_deref(), Some("r"));
            assert_eq!(rp.types, vec!["KNOWS".to_string()]);
            assert_eq!(rp.direction, RelDirection::Right);
        } else {
            panic!("expected rel pattern");
        }
    } else {
        panic!("expected MATCH");
    }
}

#[test]
fn parse_variable_length_path() {
    let q = parse_cypher("MATCH (a)-[*1..3]->(b) RETURN b").unwrap();
    if let CypherClause::Match(m) = &q.clauses[0] {
        if let PathElement::Rel(rp) = &m.patterns[0].elements[1] {
            assert_eq!(rp.min_hops, Some(1));
            assert_eq!(rp.max_hops, Some(3));
        } else {
            panic!();
        }
    }
}

#[test]
fn parse_where_with_comparison_and_logic() {
    let q = parse_cypher("MATCH (n) WHERE n.age > 18 AND n.name = 'a' RETURN n").unwrap();
    if let CypherClause::Match(m) = &q.clauses[0] {
        assert!(m.r#where.is_some());
        if let CypherExpr::BinaryOp(bo) = m.r#where.as_ref().unwrap() {
            assert_eq!(bo.op, "AND");
        } else {
            panic!();
        }
    }
}

#[test]
fn parse_function_call_and_distinct() {
    let q = parse_cypher("MATCH (n) RETURN count(DISTINCT n.id)").unwrap();
    if let CypherClause::Return(r) = &q.clauses[1] {
        if let CypherExpr::FunctionCall(fc) = &r.items[0].expr {
            assert_eq!(fc.name, "count");
            assert!(fc.distinct);
        } else {
            panic!();
        }
    }
}

#[test]
fn parse_create_with_property_map() {
    let q = parse_cypher("CREATE (n:Person {name: 'alice', age: 30})").unwrap();
    if let CypherClause::Create(c) = &q.clauses[0] {
        if let PathElement::Node(np) = &c.patterns[0].elements[0] {
            let props = np.properties.as_ref().unwrap();
            assert!(props.contains_key("name"));
            assert!(props.contains_key("age"));
        } else {
            panic!();
        }
    }
}

#[test]
fn parse_unwind_and_with() {
    let q = parse_cypher("UNWIND [1,2,3] AS x WITH x WHERE x > 1 RETURN x").unwrap();
    assert_eq!(q.clauses.len(), 3);
    if let CypherClause::Unwind(u) = &q.clauses[0] {
        assert_eq!(u.variable, "x");
    } else {
        panic!();
    }
    if let CypherClause::With(w) = &q.clauses[1] {
        assert!(w.r#where.is_some());
    } else {
        panic!();
    }
}

#[test]
fn parse_optional_match_and_set() {
    let q = parse_cypher("OPTIONAL MATCH (n) SET n.flag = true RETURN n").unwrap();
    if let CypherClause::Match(m) = &q.clauses[0] {
        assert!(m.optional);
    } else {
        panic!();
    }
    if let CypherClause::Set(s) = &q.clauses[1] {
        assert_eq!(s.items[0].operator, SetOperator::Assign);
    } else {
        panic!();
    }
}

#[test]
fn parse_detach_delete() {
    let q = parse_cypher("MATCH (n) DETACH DELETE n").unwrap();
    if let CypherClause::Delete(d) = &q.clauses[1] {
        assert!(d.detach);
    } else {
        panic!();
    }
}

#[test]
fn parse_in_list_and_starts_with() {
    let q =
        parse_cypher("MATCH (n) WHERE n.name STARTS WITH 'a' AND n.id IN [1, 2] RETURN n").unwrap();
    if let CypherClause::Match(m) = &q.clauses[0] {
        assert!(m.r#where.is_some());
    } else {
        panic!();
    }
}
