//! Canonical MCP logical session wire refs: `l_<base64url-unpadded-uuid-bytes>` (24 chars).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use crate::session_identity::LogicalSessionId;

pub const LOGICAL_SESSION_WIRE_PREFIX: &str = "l_";
pub const LOGICAL_SESSION_WIRE_TOKEN_LEN: usize = 22;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogicalSessionWireRefError {
    #[error("logical_session_ref must be `l_` + 22 URL-safe base64 chars (got {0:?})")]
    InvalidFormat(String),
    #[error(
        "legacy transport slot refs (`s0`, …) are no longer accepted; call `plasm_context` for a new `l_<token>` handle"
    )]
    LegacyTransportSlot(String),
    #[error(
        "UUID text is not accepted as logical_session_ref; use the `l_<token>` from `plasm_context`"
    )]
    RawUuidRejected(String),
}

pub fn format_logical_session_wire_ref(id: LogicalSessionId) -> String {
    format_logical_session_wire_ref_from_uuid(id.as_uuid())
}

pub fn format_logical_session_wire_ref_from_uuid(uuid: uuid::Uuid) -> String {
    let token = URL_SAFE_NO_PAD.encode(uuid.as_bytes());
    debug_assert_eq!(token.len(), LOGICAL_SESSION_WIRE_TOKEN_LEN);
    format!("{LOGICAL_SESSION_WIRE_PREFIX}{token}")
}

pub fn parse_logical_session_wire_ref(
    s: &str,
) -> Result<LogicalSessionId, LogicalSessionWireRefError> {
    let t = s.trim();
    if is_legacy_transport_slot(t) {
        return Err(LogicalSessionWireRefError::LegacyTransportSlot(
            t.to_string(),
        ));
    }
    if uuid::Uuid::parse_str(t).is_ok() {
        return Err(LogicalSessionWireRefError::RawUuidRejected(t.to_string()));
    }
    let Some(token) = t.strip_prefix(LOGICAL_SESSION_WIRE_PREFIX) else {
        return Err(LogicalSessionWireRefError::InvalidFormat(t.to_string()));
    };
    if !is_valid_wire_token(token) {
        return Err(LogicalSessionWireRefError::InvalidFormat(t.to_string()));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| LogicalSessionWireRefError::InvalidFormat(t.to_string()))?;
    if bytes.len() != 16 {
        return Err(LogicalSessionWireRefError::InvalidFormat(t.to_string()));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&bytes);
    Ok(LogicalSessionId(uuid::Uuid::from_bytes(arr)))
}

#[inline]
pub fn is_legacy_transport_slot(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('s') && s[1..].chars().all(|c| c.is_ascii_digit())
}

#[inline]
pub fn is_valid_wire_token(token: &str) -> bool {
    token.len() == LOGICAL_SESSION_WIRE_TOKEN_LEN
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Full wire ref segment (`l_<token>`) for paging/operation/resource URI namespaces.
#[inline]
pub fn is_valid_logical_session_wire_segment(s: &str) -> bool {
    let Some(token) = s.strip_prefix(LOGICAL_SESSION_WIRE_PREFIX) else {
        return false;
    };
    is_valid_wire_token(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn round_trip_known_uuid() {
        let uuid = uuid::Uuid::from_u128(1);
        let wire = format_logical_session_wire_ref_from_uuid(uuid);
        let id = parse_logical_session_wire_ref(&wire).expect("parse");
        assert_eq!(id.as_uuid(), uuid);
        assert_eq!(wire.len(), 24);
    }

    #[test]
    fn rejects_legacy_slot_and_raw_uuid() {
        assert!(matches!(
            parse_logical_session_wire_ref("s0"),
            Err(LogicalSessionWireRefError::LegacyTransportSlot(_))
        ));
        assert!(matches!(
            parse_logical_session_wire_ref("00000000-0000-0000-0000-000000000001"),
            Err(LogicalSessionWireRefError::RawUuidRejected(_))
        ));
    }

    #[test]
    fn rejects_malformed_token() {
        assert!(parse_logical_session_wire_ref("l_short").is_err());
        assert!(parse_logical_session_wire_ref("l_AAAAAAAAAAAAAAAAAAAA=").is_err());
        assert!(parse_logical_session_wire_ref("x_AAAAAAAAAAAAAAAAAAAA").is_err());
    }

    #[test]
    fn token_may_contain_dash_and_underscore() {
        let uuid = Uuid::new_v4();
        let wire = format_logical_session_wire_ref_from_uuid(uuid);
        assert!(wire.contains('l'));
        let id = parse_logical_session_wire_ref(&wire).expect("parse random uuid");
        assert_eq!(id.as_uuid(), uuid);
    }
}
