use super::error::McpOAuthError;

pub fn normalize_resource_url(raw: &str) -> Result<String, McpOAuthError> {
    let parsed = url::Url::parse(raw.trim())
        .map_err(|_| McpOAuthError::invalid_target("resource must be an absolute URL"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(McpOAuthError::invalid_target(
            "resource URL must use http or https",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| McpOAuthError::invalid_target("resource URL must include a host"))?;
    let mut normalized = parsed.clone();
    normalized
        .set_scheme(if parsed.scheme() == "https" {
            "https"
        } else {
            "http"
        })
        .map_err(|_| McpOAuthError::invalid_target("resource URL scheme is invalid"))?;
    normalized
        .set_host(Some(host.to_ascii_lowercase().as_str()))
        .map_err(|_| McpOAuthError::invalid_target("resource URL host is invalid"))?;
    let mut path = normalized.path().to_string();
    while path.ends_with('/') && path.len() > 1 {
        path.pop();
    }
    normalized.set_path(&path);
    normalized.set_query(None);
    normalized.set_fragment(None);
    Ok(normalized.to_string())
}

pub fn resolve_resource_param(
    canonical: &str,
    requested: Option<&str>,
) -> Result<String, McpOAuthError> {
    match requested {
        None => Ok(canonical.to_string()),
        Some(raw) if raw.trim().is_empty() => Ok(canonical.to_string()),
        Some(raw) => {
            let normalized = normalize_resource_url(raw)?;
            if normalized != canonical {
                return Err(McpOAuthError::invalid_target(
                    "resource parameter does not match this authorization server",
                ));
            }
            Ok(normalized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resource_lowercases_host_and_trims_slash() {
        let canonical = "https://platform.plasm.tools/plasm/mcp";
        let normalized =
            normalize_resource_url("HTTPS://Platform.Plasm.Tools/plasm/mcp/").expect("normalize");
        assert_eq!(normalized, canonical);
        assert_eq!(
            resolve_resource_param(canonical, None).expect("default"),
            canonical
        );
        assert_eq!(
            resolve_resource_param(canonical, Some("https://platform.plasm.tools/plasm/mcp"))
                .expect("match"),
            canonical
        );
    }

    #[test]
    fn reject_mismatched_resource() {
        let canonical = "https://platform.plasm.tools/plasm/mcp";
        let err = resolve_resource_param(canonical, Some("https://evil.example/mcp"))
            .expect_err("mismatch");
        assert_eq!(err.oauth_error_code(), "invalid_target");
    }
}
