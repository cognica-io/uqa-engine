//! Keep each message variant's decode temporaries in its own construction frame.

pub(crate) const fn message_tag(text: &str) -> u32 {
    let bytes = text.as_bytes();
    assert!(!bytes.is_empty());
    let mut tag = 0_u32;
    let mut index = 0;
    while index < bytes.len() {
        assert!(bytes[index] >= b'0' && bytes[index] <= b'9');
        tag = match tag.checked_mul(10) {
            Some(value) => value,
            None => panic!("protobuf tag overflow"),
        };
        tag = match tag.checked_add((bytes[index] - b'0') as u32) {
            Some(value) => value,
            None => panic!("protobuf tag overflow"),
        };
        index += 1;
    }
    assert!(tag > 0 && tag < (1 << 29));
    tag
}

pub(crate) fn merge_message<M: prost::Message + Default, E>(
    field: &mut Option<E>,
    wire_type: prost::encoding::WireType,
    input: &mut impl prost::bytes::Buf,
    context: prost::encoding::DecodeContext,
    select: fn(&mut E) -> Option<&mut M>,
    construct: fn(M) -> E,
) -> Result<(), prost::DecodeError> {
    if let Some(value) = field.as_mut().and_then(select) {
        return prost::encoding::message::merge(wire_type, value, input, context);
    }
    let mut value = M::default();
    prost::encoding::message::merge(wire_type, &mut value, input, context)?;
    *field = Some(construct(value));
    Ok(())
}

// The generated Node alternatives are all messages. Keep their original tags,
// payload types and Prost primitives; only isolate variant construction frames.
macro_rules! bounded_message_oneof {
    (
        $(#[$attribute:meta])*
        $visibility:vis enum $name:ident {
            $(#[prost(message, tag = $tag:literal)] $variant:ident($payload:ty $(,)?),)*
        }
    ) => {
        $(#[$attribute])*
        #[derive(Debug)]
        $visibility enum $name {
            $($variant($payload),)*
        }

        mod variant_tags {
            $(
                pub(super) struct $variant;
                impl $variant {
                    pub(super) const VALUE: u32 = $crate::protobuf_oneof::message_tag($tag);
                }
            )*
        }

        impl $name {
            pub fn encode(&self, output: &mut impl ::prost::bytes::BufMut) {
                match self {
                    $(Self::$variant(value) => ::prost::encoding::message::encode(
                        variant_tags::$variant::VALUE, value, output
                    ),)*
                }
            }

            pub fn encoded_len(&self) -> usize {
                match self {
                    $(Self::$variant(value) => ::prost::encoding::message::encoded_len(
                        variant_tags::$variant::VALUE, value
                    ),)*
                }
            }

            pub fn merge(
                field: &mut Option<Self>,
                tag: u32,
                wire_type: ::prost::encoding::WireType,
                input: &mut impl ::prost::bytes::Buf,
                context: ::prost::encoding::DecodeContext,
            ) -> Result<(), ::prost::DecodeError> {
                match tag {
                    $(variant_tags::$variant::VALUE => $crate::protobuf_oneof::merge_message(
                        field, wire_type, input, context,
                        |value| match value {
                            Self::$variant(value) => Some(value),
                            _ => None,
                        },
                        Self::$variant,
                    ),)*
                    _ => unreachable!(concat!("invalid ", stringify!($name), " tag: {}"), tag),
                }
            }
        }
    };
}

#[cfg(test)]
mod tests;
