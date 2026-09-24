//! Provider serialization seam. JSON exists only at encode/decode, never as
//! selection state. Each decoder receives the binding context issued with its request.
use anyhow::Result;
use serde::Serialize;

pub(crate) trait DecisionCodec {
    type Request<'a>: Serialize;
    type Context: ?Sized;
    type Output;

    fn encode(request: &Self::Request<'_>) -> Result<String> {
        // Canonical map order makes identities independent of Rust field declaration order.
        Ok(serde_json::to_string(&serde_json::to_value(request)?)?)
    }
    fn decode(context: &Self::Context, raw: &str) -> Result<Self::Output>;
}

/// serde's ordinary map decoder overwrites duplicate keys. Provider decisions
/// must reject those ambiguous bindings instead of accepting the last answer.
pub(crate) fn unique_map<'de, D, V>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    struct Visitor<V>(std::marker::PhantomData<V>);
    impl<'de, V: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<V> {
        type Value = std::collections::BTreeMap<String, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique decision keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = std::collections::BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, V>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate decision key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}
