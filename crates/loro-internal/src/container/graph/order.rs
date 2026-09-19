//! Outgoing-edge order: validated fractional keys, local destinations and results.
use fractional_index::FractionalIndex;
use loro_common::{GraphEdgeId, GraphNodeId, LoroError};
use serde::{
    de::{Error, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::fmt;

/// Resource limit for one encoded Graph position. Exceeding it fails before editing.
pub const MAX_GRAPH_POSITION_BYTES: usize = 4096;

/// A canonical fractional key. Unlike unchecked library constructors, all public
/// and serialized inputs are nonempty, bounded, and terminated by 0x80.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GraphPosition(FractionalIndex);

impl GraphPosition {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, GraphOrderError> {
        if bytes.len() > MAX_GRAPH_POSITION_BYTES {
            return Err(GraphOrderError::PositionTooLong);
        }
        if bytes.is_empty() || bytes.last() != Some(&128) {
            return Err(GraphOrderError::InvalidPosition);
        }
        Ok(Self(FractionalIndex::from_bytes(bytes)))
    }
    pub fn try_from_hex(text: &str) -> Result<Self, GraphOrderError> {
        if text.len() > MAX_GRAPH_POSITION_BYTES * 2 {
            return Err(GraphOrderError::PositionTooLong);
        }
        if text.is_empty() || text.len() % 2 != 0 || !text.is_ascii() {
            return Err(GraphOrderError::InvalidPosition);
        }
        let bytes = text
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |b: u8| {
                    (b as char)
                        .to_digit(16)
                        .ok_or(GraphOrderError::InvalidPosition)
                };
                Ok((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
            })
            .collect::<Result<Vec<_>, GraphOrderError>>()?;
        Self::try_from_bytes(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
    pub(crate) fn between(
        left: Option<&Self>,
        right: Option<&Self>,
        jitter: u8,
    ) -> Result<Self, GraphOrderError> {
        if left.zip(right).is_some_and(|(a, b)| a >= b) {
            return Err(GraphOrderError::InvalidPosition);
        }
        let key = if jitter == 0 {
            FractionalIndex::new(left.map(|p| &p.0), right.map(|p| &p.0))
        } else {
            FractionalIndex::new_jitter(
                left.map(|p| &p.0),
                right.map(|p| &p.0),
                &mut rand::thread_rng(),
                jitter,
            )
        }
        .ok_or(GraphOrderError::InvalidPosition)?;
        let mut bytes = key.as_bytes().to_vec();
        // Jitter's library representation ends in random bytes. Preserve a final
        // sentinel so even adversarially chosen valid keys remain dense/safe.
        if jitter != 0 {
            bytes.push(128);
        }
        let key = Self::try_from_bytes(bytes)?;
        if left.is_some_and(|p| p >= &key) || right.is_some_and(|p| &key >= p) {
            return Err(GraphOrderError::InvalidPosition);
        }
        Ok(key)
    }
    pub(crate) fn evenly(
        left: Option<&Self>,
        right: Option<&Self>,
        n: usize,
        jitter: u8,
    ) -> Result<Vec<Self>, GraphOrderError> {
        if jitter == 0 {
            return FractionalIndex::generate_n_evenly(left.map(|p| &p.0), right.map(|p| &p.0), n)
                .ok_or(GraphOrderError::InvalidPosition)?
                .into_iter()
                .map(|key| Self::try_from_bytes(key.as_bytes().to_vec()))
                .collect();
        }
        fn generate(
            left: Option<&GraphPosition>,
            right: Option<&GraphPosition>,
            n: usize,
            jitter: u8,
            out: &mut Vec<GraphPosition>,
        ) -> Result<(), GraphOrderError> {
            if n == 0 {
                return Ok(());
            }
            let mid = GraphPosition::between(left, right, jitter)?;
            generate(left, Some(&mid), n / 2, jitter, out)?;
            out.push(mid.clone());
            generate(Some(&mid), right, n - n / 2 - 1, jitter, out)
        }
        let mut result = Vec::with_capacity(n);
        generate(left, right, n, jitter, &mut result)?;
        Ok(result)
    }
}
impl fmt::Display for GraphPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl Serialize for GraphPosition {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&self.to_string())
        } else {
            s.serialize_bytes(self.as_bytes())
        }
    }
}
impl<'de> Deserialize<'de> for GraphPosition {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = GraphPosition;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a bounded Graph fractional key ending in 80")
            }
            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                GraphPosition::try_from_hex(value).map_err(E::custom)
            }
            fn visit_bytes<E: Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                if value.len() > MAX_GRAPH_POSITION_BYTES {
                    return Err(E::custom(GraphOrderError::PositionTooLong));
                }
                GraphPosition::try_from_bytes(value.to_vec()).map_err(E::custom)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq
                    .size_hint()
                    .is_some_and(|n| n > MAX_GRAPH_POSITION_BYTES)
                {
                    return Err(A::Error::custom(GraphOrderError::PositionTooLong));
                }
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element()? {
                    if bytes.len() == MAX_GRAPH_POSITION_BYTES {
                        return Err(A::Error::custom(GraphOrderError::PositionTooLong));
                    }
                    bytes.push(byte);
                }
                GraphPosition::try_from_bytes(bytes).map_err(A::Error::custom)
            }
        }
        if d.is_human_readable() {
            d.deserialize_str(KeyVisitor)
        } else {
            d.deserialize_bytes(KeyVisitor)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphOrderTarget {
    Start,
    End,
    Before(GraphEdgeId),
    After(GraphEdgeId),
}

#[derive(Debug, thiserror::Error)]
pub enum GraphOrderError {
    #[error("Graph source or endpoint does not exist in this graph")]
    MissingNode,
    #[error("Graph edge is missing or not visible")]
    EdgeNotVisible,
    #[error("Graph order anchor is missing or not visible")]
    AnchorNotVisible,
    #[error("Graph order anchor belongs to another source")]
    CrossSource,
    #[error("Invalid Graph position encoding or interval")]
    InvalidPosition,
    #[error("Graph position exceeds 4096 bytes")]
    PositionTooLong,
    #[error(transparent)]
    Engine(#[from] LoroError),
}

/// Auxiliary writes compete with concurrent user writes using ordinary LWW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphReorderOutcome {
    pub changed: bool,
    pub auxiliary_updates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedGraphEdge {
    pub edge_id: GraphEdgeId,
    pub target: GraphNodeId,
    pub position: GraphPosition,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn canonical_keys_are_dense_even_at_adversarial_prefixes() {
        let mut keys = vec![GraphPosition::default()];
        for length in 1..=4 {
            for mut digits in 0..5usize.pow(length) {
                let mut bytes = Vec::new();
                for _ in 0..length {
                    bytes.push([0, 127, 128, 129, 255][digits % 5]);
                    digits /= 5;
                }
                bytes.push(128);
                keys.push(GraphPosition::try_from_bytes(bytes).unwrap());
            }
        }
        keys.sort();
        for adjacent in keys.windows(2) {
            for jitter in [0, 1, 8, 255] {
                let key =
                    GraphPosition::between(Some(&adjacent[0]), Some(&adjacent[1]), jitter).unwrap();
                assert!(adjacent[0] < key && key < adjacent[1]);
                assert_eq!(key.as_bytes().last(), Some(&128));
            }
        }
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x10_20_30);
        for _ in 0..1000 {
            let a = rng.gen_range(0..keys.len() - 1);
            let b = rng.gen_range(a + 1..keys.len());
            let allocated = GraphPosition::evenly(Some(&keys[a]), Some(&keys[b]), 17, 4).unwrap();
            assert!(keys[a] < allocated[0] && allocated[16] < keys[b]);
            assert!(allocated.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn malformed_and_oversized_keys_fail_before_algorithm_entry() {
        for text in ["", "8", "00", "zz", "808", "é80", "８０"] {
            assert!(GraphPosition::try_from_hex(text).is_err());
            assert!(serde_json::from_str::<GraphPosition>(&format!("\"{text}\"")).is_err());
        }
        let max = vec![128; MAX_GRAPH_POSITION_BYTES];
        let valid = GraphPosition::try_from_bytes(max.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<GraphPosition>(&serde_json::to_string(&valid).unwrap()).unwrap(),
            valid
        );
        assert_eq!(
            postcard::from_bytes::<GraphPosition>(&postcard::to_stdvec(&valid).unwrap()).unwrap(),
            valid
        );
        assert!(matches!(
            GraphPosition::try_from_bytes(vec![128; MAX_GRAPH_POSITION_BYTES + 1]),
            Err(GraphOrderError::PositionTooLong)
        ));
        assert!(postcard::from_bytes::<GraphPosition>(
            &postcard::to_stdvec(&vec![128u8; MAX_GRAPH_POSITION_BYTES + 1]).unwrap()
        )
        .is_err());
        let lower = GraphPosition::try_from_bytes(
            [vec![255; MAX_GRAPH_POSITION_BYTES - 1], vec![128]].concat(),
        )
        .unwrap();
        assert!(matches!(
            GraphPosition::between(Some(&lower), None, 0),
            Err(GraphOrderError::PositionTooLong)
        ));
    }
}
