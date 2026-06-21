//! Flatten nested `embedded_entities` trees for session-graph insert (CEP-10 depth cap).

use plasm_core::MAX_FROM_PARENT_GET_EMBED_DEPTH;

use crate::decoder::DecodedEntity;

/// Descendants first (deepest embed depth highest), each node with `embedded_entities` cleared.
pub fn flatten_decoded_embed_descendants(root: &DecodedEntity) -> Vec<DecodedEntity> {
    let mut nodes = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    while let Some((ent, depth)) = stack.pop() {
        if depth >= MAX_FROM_PARENT_GET_EMBED_DEPTH {
            continue;
        }
        for child in ent.embedded_entities.iter().rev() {
            stack.push((child.clone(), depth + 1));
        }
        let mut node = ent;
        node.embedded_entities = Vec::new();
        nodes.push((depth, node));
    }
    nodes.sort_by(|(a, _), (b, _)| b.cmp(a));
    nodes.into_iter().map(|(_, e)| e).collect()
}
