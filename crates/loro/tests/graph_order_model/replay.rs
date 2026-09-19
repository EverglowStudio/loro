//! Delta-debug a deterministic decision tape, never mutate signed wire histories.
//! Replaying a shortened tape still generates operations through valid public APIs.

/// Return a 1-minimal subsequence for the caller's exact failure predicate.
/// The predicate is responsible for distinguishing the original failure from a
/// different bug. There is no assertion relaxation or invalid-input acceptance.
pub fn minimize<T: Clone>(tape: &[T], mut same_failure: impl FnMut(&[T]) -> bool) -> Vec<T> {
    assert!(
        same_failure(tape),
        "cannot minimize a non-reproducing failure"
    );
    if same_failure(&[]) {
        return vec![];
    }
    let mut current = tape.to_vec();
    let mut chunks = 2;
    while current.len() > 1 {
        let size = current.len().div_ceil(chunks);
        let mut reduced = false;
        for start in (0..current.len()).step_by(size) {
            let end = (start + size).min(current.len());
            let candidate: Vec<_> = current[..start]
                .iter()
                .chain(&current[end..])
                .cloned()
                .collect();
            if same_failure(&candidate) {
                current = candidate;
                chunks = chunks.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if chunks >= current.len() {
                break;
            }
            chunks = (chunks * 2).min(current.len());
        }
    }
    current
}

pub fn encode(tape: &[u64]) -> String {
    tape.iter()
        .map(|seed| format!("{seed:016x}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn decode(text: &str) -> Vec<u64> {
    if text.is_empty() {
        return vec![];
    }
    text.split(',')
        .map(|word| u64::from_str_radix(word, 16).expect("replay seeds must be hexadecimal u64s"))
        .collect()
}

#[test]
fn shrinking_preserves_the_exact_required_subsequence() {
    let original: Vec<u64> = (0..64).collect();
    let failure = |tape: &[u64]| tape.windows(2).any(|pair| pair == [17, 18]);
    let result = minimize(&original, failure);
    assert_eq!(result, vec![17, 18]);
    for index in 0..result.len() {
        let mut smaller = result.clone();
        smaller.remove(index);
        assert!(!failure(&smaller));
    }
    assert_eq!(decode(&encode(&result)), result);
}

#[test]
fn replay_encoding_is_lossless_for_large_seeds_and_empty_failures() {
    let tape = vec![0, u64::MAX, (1 << 54) + 3];
    assert_eq!(decode(&encode(&tape)), tape);
    assert_eq!(decode(&encode(&[])), Vec::<u64>::new());
    assert!(minimize(&tape, |_| true).is_empty());
}
