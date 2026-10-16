use prost::Message;

#[derive(Clone, PartialEq, prost::Message, serde::Serialize)]
pub struct Payload {
    #[prost(int32, tag = "1")]
    pub number: i32,
    #[prost(string, tag = "2")]
    pub text: String,
    #[prost(message, optional, boxed, tag = "3")]
    pub child: Option<Box<Payload>>,
}

mod reference {
    #[derive(Clone, PartialEq, prost::Message, serde::Serialize)]
    pub struct Envelope {
        #[prost(oneof = "Choice", tags = "1, 129")]
        pub choice: Option<Choice>,
    }

    #[derive(Clone, PartialEq, prost::Oneof, serde::Serialize)]
    pub enum Choice {
        #[prost(message, tag = "1")]
        Direct(super::Payload),
        #[prost(message, tag = "129")]
        Boxed(Box<super::Payload>),
    }
}

mod bounded {
    #[derive(Clone, PartialEq, prost::Message, serde::Serialize)]
    pub struct Envelope {
        #[prost(oneof = "Choice", tags = "1, 129")]
        pub choice: Option<Choice>,
    }

    bounded_message_oneof! {
        #[derive(Clone, PartialEq, serde::Serialize)]
        pub enum Choice {
            #[prost(message, tag = "1")]
            Direct(super::Payload),
            #[prost(message, tag = "129")]
            Boxed(Box<super::Payload>),
        }
    }
}

fn encoded(boxed: bool, number: i32, text: &str) -> Vec<u8> {
    let value = Payload {
        number,
        text: text.into(),
        child: None,
    };
    let choice = if boxed {
        reference::Choice::Boxed(Box::new(value))
    } else {
        reference::Choice::Direct(value)
    };
    reference::Envelope {
        choice: Some(choice),
    }
    .encode_to_vec()
}

fn compare_merge(initial: &[u8], incoming: &[u8]) {
    let mut reference = reference::Envelope::decode(initial).unwrap();
    let mut bounded = bounded::Envelope::decode(initial).unwrap();
    let mut original_input = incoming;
    let mut bounded_input = incoming;
    let original_result = reference
        .merge(&mut original_input)
        .map_err(|error| error.to_string());
    let bounded_result = bounded
        .merge(&mut bounded_input)
        .map_err(|error| error.to_string());
    assert_eq!(bounded_result, original_result, "incoming: {incoming:?}");
    assert_eq!(bounded_input, original_input, "remaining bytes");
    assert_eq!(
        serde_json::to_value(&bounded).unwrap(),
        serde_json::to_value(&reference).unwrap(),
        "retained state"
    );
    assert_eq!(bounded.encoded_len(), reference.encoded_len());
    assert_eq!(bounded.encode_to_vec(), reference.encode_to_vec());
    assert_eq!(format!("{bounded:?}"), format!("{reference:?}"));
}

#[test]
fn message_oneof_matches_prost_wire_state_errors_and_consumption() {
    let direct_number = encoded(false, 17, "");
    let direct_text = encoded(false, 0, "retained");
    let boxed_number = encoded(true, -9, "");
    let boxed_text = encoded(true, 0, "boxed");
    let mut corpus = vec![
        Vec::new(),
        encoded(false, 0, ""),
        encoded(true, 0, ""),
        direct_number.clone(),
        direct_text.clone(),
        boxed_number.clone(),
        boxed_text.clone(),
        // Known oneof tags with wrong wire types, and an unknown field.
        vec![0x08, 0x01],
        vec![0x88, 0x08, 0x01],
        vec![0x3a, 0x03, 0x01, 0x02, 0x03],
        // The inner int changes before the truncated string fails to decode.
        vec![0x0a, 0x05, 0x08, 0x63, 0x12, 0x02, b'x'],
        vec![0x8a, 0x08, 0x05, 0x08, 0x63, 0x12, 0x02, b'x'],
    ];
    for first in [&direct_number, &boxed_number] {
        for second in [&direct_text, &boxed_text] {
            corpus.push([first.as_slice(), second.as_slice()].concat());
        }
    }
    for initial in [&[][..], direct_number.as_slice(), boxed_text.as_slice()] {
        for input in &corpus {
            for end in 0..=input.len() {
                compare_merge(initial, &input[..end]);
            }
        }
    }
}

#[test]
fn message_oneof_preserves_prost_recursion_limit() {
    let mut payload = Payload::default();
    for _ in 0..120 {
        payload = Payload {
            child: Some(Box::new(payload)),
            ..Default::default()
        };
    }
    let input = reference::Envelope {
        choice: Some(reference::Choice::Direct(payload)),
    }
    .encode_to_vec();
    assert!(reference::Envelope::decode(input.as_slice())
        .unwrap_err()
        .to_string()
        .contains("recursion limit"));
    compare_merge(&[], &input);
    compare_merge(&encoded(false, 7, "prior"), &input);
    compare_merge(&encoded(true, 7, "prior"), &input);
}

#[path = "../protobuf_codegen.rs"]
mod codegen;

#[test]
fn node_postprocessing_preserves_generated_tags_types_and_other_messages() {
    let source = include_str!("../protobuf.rs");
    let generated = source
        .replacen("    bounded_message_oneof! {\n", "", 1)
        .replacen(
            "#[derive(Clone, PartialEq)]",
            "#[derive(Clone, PartialEq, ::prost::Oneof)]",
            1,
        )
        .replacen("\n    }\n}\n", "\n}\n", 1);
    assert_eq!(codegen::bounded_node_oneof(generated).unwrap(), source);
    assert!(codegen::bounded_node_oneof(source.into()).is_err());
    assert!(codegen::bounded_node_oneof("pub mod node {}".into()).is_err());
}
