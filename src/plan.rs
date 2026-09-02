//! The plan name on the usage line, derived the way the cpa-claude-statusline CLIProxyAPI
//! plugin derives it from the same profile response, so both modes agree on the word.

/// First match wins. The two flags decide max and pro because the organization type alone does
/// not separate them; team needs an active subscription; enterprise and anything newer fall
/// through to the organization type. The local `~/.claude.json` carries neither flag nor the
/// subscription status, so from that file only the last rule applies.
#[allow(dead_code)]
pub fn derive(
    organization_type: Option<&str>,
    has_claude_max: Option<bool>,
    has_claude_pro: Option<bool>,
    subscription_status: Option<&str>,
) -> Option<String> {
    if has_claude_max == Some(true) {
        return Some("max".to_string());
    }
    if has_claude_pro == Some(true) {
        return Some("pro".to_string());
    }
    let org = organization_type
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(str::to_ascii_lowercase);
    let active = subscription_status
        .map(str::trim)
        .is_some_and(|s| s.eq_ignore_ascii_case("active"));
    if org.as_deref() == Some("claude_team") && active {
        return Some("team".to_string());
    }
    if has_claude_max == Some(false) && has_claude_pro == Some(false) {
        return Some("free".to_string());
    }
    org.map(|o| o.strip_prefix("claude_").unwrap_or(&o).to_string())
}

#[cfg(test)]
mod tests {
    use super::derive;

    #[test]
    fn rules_apply_in_order() {
        assert_eq!(
            derive(Some("claude_max"), Some(true), Some(false), Some("active")).as_deref(),
            Some("max")
        );
        assert_eq!(
            derive(Some("claude_pro"), Some(false), Some(true), None).as_deref(),
            Some("pro")
        );
        assert_eq!(
            derive(
                Some("claude_team"),
                Some(false),
                Some(false),
                Some("active")
            )
            .as_deref(),
            Some("team")
        );
        assert_eq!(
            derive(
                Some("claude_team"),
                Some(false),
                Some(false),
                Some("past_due")
            )
            .as_deref(),
            Some("free")
        );
        assert_eq!(
            derive(None, Some(false), Some(false), None).as_deref(),
            Some("free")
        );
        assert_eq!(
            derive(Some("claude_enterprise"), None, None, None).as_deref(),
            Some("enterprise")
        );
        assert_eq!(
            derive(Some(" Business "), None, None, None).as_deref(),
            Some("business")
        );
        assert_eq!(derive(Some(""), None, None, None), None);
        assert_eq!(derive(None, None, None, None), None);
    }

    #[test]
    fn local_file_shape_keeps_the_current_plan_chip() {
        assert_eq!(
            derive(Some("claude_max"), None, None, None).as_deref(),
            Some("max")
        );
        assert_eq!(
            derive(Some("claude_team"), None, None, None).as_deref(),
            Some("team")
        );
    }
}
