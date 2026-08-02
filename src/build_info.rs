//! Build provenance, populated by `build.rs`.
//!
//! `PPK2TUI_GIT_SHA` is `"unknown"` when the source tree has no git metadata
//! and the build did not pass one in — notably Docker builds, which exclude
//! `.git/` from the context (see `.dockerignore`) and set the variable instead.

/// Long form, shown by `--version`.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("PPK2TUI_GIT_SHA"),
    ", built ",
    env!("PPK2TUI_BUILD_DATE"),
    ")"
);

/// Compact form for the TUI chart header.
pub const TUI_LABEL: &str = concat!(
    "v",
    env!("CARGO_PKG_VERSION"),
    " · ",
    env!("PPK2TUI_GIT_SHA"),
    " · ",
    env!("PPK2TUI_BUILD_DATE")
);

#[cfg(test)]
mod tests {
    use super::*;

    const GIT_SHA: &str = env!("PPK2TUI_GIT_SHA");
    const BUILD_DATE: &str = env!("PPK2TUI_BUILD_DATE");

    #[test]
    fn build_date_is_iso_yyyy_mm_dd() {
        let parts: Vec<&str> = BUILD_DATE.split('-').collect();
        assert_eq!(parts.len(), 3, "expected YYYY-MM-DD, got {BUILD_DATE}");
        assert_eq!(parts[0].len(), 4, "expected 4-digit year in {BUILD_DATE}");
        let year: u32 = parts[0].parse().expect("year should parse");
        let month: u32 = parts[1].parse().expect("month should parse");
        let day: u32 = parts[2].parse().expect("day should parse");
        assert!(year >= 2024, "implausible build year {year}");
        assert!((1..=12).contains(&month), "bad month {month}");
        assert!((1..=31).contains(&day), "bad day {day}");
    }

    #[test]
    fn git_sha_is_populated() {
        assert!(!GIT_SHA.is_empty(), "git sha should never be empty");
    }

    #[test]
    fn version_strings_embed_the_crate_version() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")));
        assert!(TUI_LABEL.contains(env!("CARGO_PKG_VERSION")));
        assert!(TUI_LABEL.contains(GIT_SHA));
        assert!(TUI_LABEL.contains(BUILD_DATE));
    }
}
