//! OpenAI `o200k_base` BPE length via `riptoken` and the stock [`VOCAB`](include_bytes!) bytes.
//! Used for prompt budgeting heuristics (local, no network at runtime; ~GPT‑4/4o class).

use base64::Engine;
use riptoken::CoreBPE;
use rustc_hash::FxHashMap;
use std::sync::OnceLock;

/// Vocabulary: OpenAI public blob, SHA-256 per `tiktoken` (`446a9…1a2d`); 3.4M text file.
const VOCAB: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/o200k_base.tiktoken"
));

/// `tiktoken` / `o200k_base` merge regex — same `|` order as
/// <https://github.com/openai/tiktoken/blob/main/tiktoken_ext/openai_public.py> `o200k_base()`.
const O200K_PAT: &str = r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+(?!\S)|\s+";

static BPE: OnceLock<CoreBPE> = OnceLock::new();

/// Token count of `text` with `o200k_base` ordinary encoding (no special tokens), suitable for
/// rough comparison with API billing on recent OpenAI models.
pub fn o200k_token_count(text: &str) -> usize {
    bpe().encode_ordinary(text).len()
}

fn bpe() -> &'static CoreBPE {
    BPE.get_or_init(|| {
        let enc = load_mergeable_ranks();
        let specials: FxHashMap<String, riptoken::Rank> = FxHashMap::default();
        CoreBPE::new(enc, specials, O200K_PAT)
            .expect("o200k BaseBPE: stock vocab + o200k pattern (see riptoken)")
    })
}

fn load_mergeable_ranks() -> FxHashMap<Vec<u8>, riptoken::Rank> {
    let s = std::str::from_utf8(VOCAB).expect("o200k_base.tiktoken is UTF-8 (ASCII b64 + ranks)");
    let mut out: FxHashMap<Vec<u8>, riptoken::Rank> = FxHashMap::default();
    for line in s.lines() {
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let b64 = it.next().expect("tiktoken line: b64");
        let rank: u32 = it
            .next()
            .and_then(|r| r.parse().ok())
            .expect("tiktoken line: rank");
        if it.next().is_some() {
            panic!("tiktoken line: expected two fields, got: {line:?}");
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap_or_else(|e| panic!("tiktoken b64: {e} (line {line:?})"));
        out.insert(bytes, rank);
    }
    out
}
