//! The plan name on the usage line, read from the profile endpoint's organization type and
//! plan flags.

/// First match wins. The two flags decide max and pro because the organization type alone
/// does not separate them. Any other named organization names the plan, enterprise and team
/// included, so a seat whose flags are both false is not read as free just because it is
/// neither max nor pro. Both flags false with no organization, or a free one, is free. The
/// local `~/.claude.json` carries neither flag, so from that file only the organization rule
/// applies.
pub fn derive(
    organization_type: Option<&str>,
    has_claude_max: Option<bool>,
    has_claude_pro: Option<bool>,
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
        .map(|o| o.to_ascii_lowercase())
        .map(|o| o.strip_prefix("claude_").unwrap_or(&o).to_string());
    if let Some(org) = org.as_deref().filter(|o| *o != "free") {
        return Some(org.to_string());
    }
    if has_claude_max == Some(false) && has_claude_pro == Some(false) {
        return Some("free".to_string());
    }
    org
}

/// The chip text for a plan and its rate-limit tier. `derive` names the family; this names the
/// tier a user actually runs at, which is what decides how far the windows go.
pub fn label(plan: &str, rate_limit_tier: Option<&str>) -> String {
    let mut chars = plan.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut label: String = first.to_uppercase().chain(chars).collect();
    if let Some(multiplier) = rate_limit_tier.and_then(multiplier) {
        label.push(' ');
        label.push_str(&multiplier);
    }
    label
}

/// The `_<digits>x` suffix of a tier name: `20x` from `default_claude_max_20x`, nothing from
/// `default_claude_pro`. Matched on the trimmed, lowercased tier, so `_20X` reads as `20x` too.
fn multiplier(tier: &str) -> Option<String> {
    let tier = tier.trim().to_ascii_lowercase();
    let (_, suffix) = tier.rsplit_once('_')?;
    let digits = suffix.strip_suffix('x')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| suffix.to_string())
}

#[cfg(test)]
mod tests {
    use super::{derive, label};

    #[test]
    fn rules_apply_in_order() {
        assert_eq!(
            derive(Some("claude_max"), Some(true), Some(false)).as_deref(),
            Some("max")
        );
        assert_eq!(
            derive(Some("claude_pro"), Some(false), Some(true)).as_deref(),
            Some("pro")
        );
        assert_eq!(
            derive(Some("claude_team"), Some(false), Some(false)).as_deref(),
            Some("team")
        );
        // The shape the profile endpoint returns for an enterprise seat.
        assert_eq!(
            derive(Some("claude_enterprise"), Some(false), Some(false)).as_deref(),
            Some("enterprise")
        );
        assert_eq!(
            derive(Some("claude_enterprise"), None, None).as_deref(),
            Some("enterprise")
        );
        assert_eq!(
            derive(Some(" Business "), None, None).as_deref(),
            Some("business")
        );
        assert_eq!(
            derive(None, Some(false), Some(false)).as_deref(),
            Some("free")
        );
        assert_eq!(
            derive(Some("claude_free"), Some(false), Some(false)).as_deref(),
            Some("free")
        );
        assert_eq!(
            derive(Some(""), Some(false), Some(false)).as_deref(),
            Some("free")
        );
        assert_eq!(
            derive(Some("claude_free"), None, None).as_deref(),
            Some("free")
        );
        assert_eq!(derive(Some(""), None, None), None);
        assert_eq!(derive(None, None, None), None);
    }

    #[test]
    fn local_file_shape_keeps_the_current_plan_chip() {
        assert_eq!(
            derive(Some("claude_max"), None, None).as_deref(),
            Some("max")
        );
        assert_eq!(
            derive(Some("claude_team"), None, None).as_deref(),
            Some("team")
        );
    }

    #[test]
    fn label_joins_the_family_and_the_multiplier() {
        assert_eq!(label("max", Some("default_claude_max_20x")), "Max 20x");
        assert_eq!(label("max", Some("default_claude_max_5x")), "Max 5x");
        assert_eq!(label("max", Some(" default_claude_max_20x ")), "Max 20x");
        assert_eq!(label("max", Some("default_claude_max_20X")), "Max 20x");
        assert_eq!(label("max", Some(" DEFAULT_CLAUDE_MAX_5X ")), "Max 5x");
        assert_eq!(label("pro", Some("default_claude_pro")), "Pro");
        assert_eq!(label("team", None), "Team");
    }

    #[test]
    fn label_needs_digits_before_the_x_and_a_family_before_anything() {
        assert_eq!(label("max", Some("default_claude_max_x")), "Max");
        assert_eq!(label("max", Some("default_claude_max_20")), "Max");
        assert_eq!(label("max", Some("20x")), "Max");
        assert_eq!(label("", None), "");
        assert_eq!(label("", Some("default_claude_max_20x")), "");
    }
}
