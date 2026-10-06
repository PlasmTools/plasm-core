//! How outbound API credentials are resolved: deployment env vars vs delegated per-principal broker.
//!
//! Set `PLASM_AUTH_RESOLUTION` to `delegated` when execute sessions must carry a stable `principal`
//! (e.g. end-user id) for multi-tenant credential lookup. Default and unknown values keep **`env`**
//! resolution so local development and CI stay unchanged.

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum AuthResolutionError {
    #[error("a non-empty principal is required when PLASM_AUTH_RESOLUTION=delegated")]
    PrincipalRequired,
}

/// Credential resolution strategy for HTTP execution (see [`crate::auth::AuthResolver`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthResolutionMode {
    /// Read secrets from the configured [`crate::auth::SecretProvider`] (default: env vars named in CGS).
    Env,
    /// Require a per-session principal; credentials are resolved by a broker (future work hooks here).
    Delegated,
}

/// Parse `PLASM_AUTH_RESOLUTION`. Missing, empty, `env`, or unknown values → [`AuthResolutionMode::Env`]
/// (unknown logs a warning).
pub fn auth_resolution_mode_from_env() -> AuthResolutionMode {
    auth_resolution_mode_from_str(std::env::var("PLASM_AUTH_RESOLUTION").ok().as_deref())
}

/// Parse a raw value (e.g. for tests). `None`, empty, and `env` → [`AuthResolutionMode::Env`];
/// `delegated` → [`AuthResolutionMode::Delegated`].
pub fn auth_resolution_mode_from_str(raw: Option<&str>) -> AuthResolutionMode {
    let s = raw.map(str::trim).filter(|s| !s.is_empty());
    match s.map(|x| x.to_ascii_lowercase()).as_deref() {
        None | Some("env") => AuthResolutionMode::Env,
        Some("delegated") => AuthResolutionMode::Delegated,
        Some(other) => {
            tracing::warn!(
                value = %other,
                "unknown PLASM_AUTH_RESOLUTION; using env"
            );
            AuthResolutionMode::Env
        }
    }
}

/// Returns `Err` when [`AuthResolutionMode::Delegated`] is active but no non-empty principal was given.
pub fn validate_principal_for_mode(
    mode: AuthResolutionMode,
    principal: Option<&str>,
) -> Result<(), AuthResolutionError> {
    match mode {
        AuthResolutionMode::Env => Ok(()),
        AuthResolutionMode::Delegated => {
            let ok = principal.map(|p| !p.trim().is_empty()).unwrap_or(false);
            if ok {
                Ok(())
            } else {
                Err(AuthResolutionError::PrincipalRequired)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_str_variants() {
        assert_eq!(auth_resolution_mode_from_str(None), AuthResolutionMode::Env);
        assert_eq!(
            auth_resolution_mode_from_str(Some("")),
            AuthResolutionMode::Env
        );
        assert_eq!(
            auth_resolution_mode_from_str(Some("  env  ")),
            AuthResolutionMode::Env
        );
        assert_eq!(
            auth_resolution_mode_from_str(Some("DELEGATED")),
            AuthResolutionMode::Delegated
        );
    }

    #[test]
    fn delegated_requires_principal() {
        assert!(validate_principal_for_mode(AuthResolutionMode::Delegated, None).is_err());
        assert!(validate_principal_for_mode(AuthResolutionMode::Delegated, Some("")).is_err());
        assert!(validate_principal_for_mode(AuthResolutionMode::Delegated, Some("   ")).is_err());
        assert!(validate_principal_for_mode(AuthResolutionMode::Delegated, Some("u1")).is_ok());
    }

    #[test]
    fn env_ignores_principal() {
        assert!(validate_principal_for_mode(AuthResolutionMode::Env, None).is_ok());
        assert!(validate_principal_for_mode(AuthResolutionMode::Env, Some("")).is_ok());
    }
}
