//! Convert CGS [`plasm_core::JsonPathSegment`] paths into decode [`PathExpr`](crate::decoder::PathExpr).

use crate::decoder::{PathExpr, PathSegment};
use plasm_core::JsonPathSegment;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JsonPathError {
    #[error("JSON path wildcard segment {segment_index} must use `wildcard: true`")]
    InvalidWildcard { segment_index: usize },
}

/// Build a [`PathExpr`] for nested GET decode (`RelationMaterialization::FromParentGet`).
pub fn path_expr_from_json_segments(
    segments: &[JsonPathSegment],
) -> Result<PathExpr, JsonPathError> {
    let mut segs = Vec::with_capacity(segments.len());
    for (segment_index, s) in segments.iter().enumerate() {
        match s {
            JsonPathSegment::Key { key } => {
                segs.push(PathSegment::Key { name: key.clone() });
            }
            JsonPathSegment::Wildcard { wildcard } => {
                if !wildcard {
                    return Err(JsonPathError::InvalidWildcard { segment_index });
                }
                segs.push(PathSegment::Wildcard);
            }
        }
    }
    Ok(PathExpr::new(segs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_wildcard_reports_its_segment_index() {
        let segments = [
            JsonPathSegment::Key {
                key: "items".into(),
            },
            JsonPathSegment::Wildcard { wildcard: false },
        ];

        assert_eq!(
            path_expr_from_json_segments(&segments),
            Err(JsonPathError::InvalidWildcard { segment_index: 1 })
        );
    }
}
