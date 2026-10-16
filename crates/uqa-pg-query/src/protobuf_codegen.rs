//! Deterministic postprocessing of Prost's generated PostgreSQL Node oneof.

pub(crate) fn bounded_node_oneof(mut source: String) -> std::io::Result<String> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unexpected generated Node oneof",
        )
    };
    let module = "pub mod node {\n";
    let start = source.find(module).ok_or_else(invalid)? + module.len();
    let end = start + source[start..].find("\n}\n").ok_or_else(invalid)?;
    let body = &source[start..end];
    let derive = "#[derive(Clone, PartialEq, ::prost::Oneof)]";
    if body.matches(derive).count() != 1 || !body.contains("pub enum Node {") {
        return Err(invalid());
    }
    let body = body.replace(derive, "#[derive(Clone, PartialEq)]");
    source.replace_range(
        start..end,
        &format!("    bounded_message_oneof! {{\n{body}\n    }}"),
    );
    Ok(source)
}
