//! Flatten nested `embedded_entities` trees for session-graph insert (CEP-10 depth cap).

use plasm_core::MAX_FROM_PARENT_GET_EMBED_DEPTH;

use crate::decoder::DecodedEntity;

/// Descendants first (deepest embed depth highest), each node with `embedded_entities` cleared.
pub fn flatten_decoded_embed_descendants(root: DecodedEntity) -> Vec<DecodedEntity> {
    let mut nodes = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((mut ent, depth)) = stack.pop() {
        if depth >= MAX_FROM_PARENT_GET_EMBED_DEPTH {
            continue;
        }
        for child in ent.embedded_entities.drain(..).rev() {
            stack.push((child, depth + 1));
        }
        nodes.push((depth, ent));
    }
    nodes.sort_by(|(a, _), (b, _)| b.cmp(a));
    nodes.into_iter().map(|(_, e)| e).collect()
}
