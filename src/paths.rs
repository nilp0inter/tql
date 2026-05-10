//! Path sanitization (DESIGN.md §10) and link-tag validation (§5).
//!
//! Pure, no I/O. Used by the post-processor and by the script-host's output
//! validator.

use unicode_normalization::UnicodeNormalization;

/// Per-component byte cap (NFC, UTF-8).
pub const MAX_COMPONENT_BYTES: usize = 200;
/// Hard cap on number of `/`-separated components in one relative path.
pub const MAX_PATH_COMPONENTS: usize = 32;
/// Soft cap on a `link:` tag's byte length (warn-only per §5).
pub const SOFT_MAX_LINK_TAG_BYTES: usize = 256;
/// Soft cap on number of link tags per torrent (warn-only per §5).
pub const SOFT_MAX_LINK_TAGS: usize = 16;
/// Reserved subdirectory under `<library_root>` (§4).
pub const RESERVED_FIRST_COMPONENT: &str = ".metadata";
/// Linux PATH_MAX. We don't pull libc just for this.
pub const PATH_MAX: usize = 4096;
/// Required prefix for link tags (§5).
pub const LINK_TAG_PREFIX: &str = "link:";

/// Knobs for sanitization. Mirrors the `[linking]` config block.
#[derive(Debug, Clone, Copy)]
pub struct SanitizeOpts {
    pub windows_compat: bool,
}

impl Default for SanitizeOpts {
    fn default() -> Self {
        Self { windows_compat: true }
    }
}

/// Apply §10 to a single path component.
///
/// Locale-independent, case-preserving. Always returns a non-empty string.
pub fn sanitize_component(input: &str, opts: &SanitizeOpts) -> String {
    // 1. NFC normalize.
    let nfc: String = input.nfc().collect();

    // 2. Trim leading/trailing whitespace.
    let trimmed = nfc.trim();

    // 3. Replace NUL and `/` with `_`.
    let mut out: String = trimmed
        .chars()
        .map(|c| match c {
            '\0' | '/' => '_',
            _ => c,
        })
        .collect();

    // 4. Windows-compat substitutions.
    if opts.windows_compat {
        out = out
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' => '_',
                _ => c,
            })
            .collect();

        // Trailing run of `.` or space → single `_`.
        let trimmed_end = out.trim_end_matches(|c: char| c == '.' || c == ' ');
        if trimmed_end.len() != out.len() {
            let keep = trimmed_end.len();
            out.truncate(keep);
            out.push('_');
        }

        // Windows reserved names: prepend `_`.
        if is_windows_reserved(&out) {
            out.insert(0, '_');
        }
    }

    // 5. Truncate to MAX_COMPONENT_BYTES (UTF-8 safe).
    if out.len() > MAX_COMPONENT_BYTES {
        let mut cut = MAX_COMPONENT_BYTES;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }

    // 6. Empty → `_`.
    if out.is_empty() {
        out.push('_');
    }

    out
}

fn is_windows_reserved(s: &str) -> bool {
    // Match on the stem (before the first `.`), case-insensitive.
    let stem = s.split('.').next().unwrap_or(s);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
            | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// Errors a `link:` tag can fail validation with (§5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTagError {
    /// Missing literal `link:` prefix.
    MissingPrefix,
    /// Relative path is empty (after stripping the prefix).
    EmptyPath,
    /// Path begins with `/` (absolute).
    AbsolutePath,
    /// A component is `.`, `..`, or empty.
    InvalidComponent,
    /// First component is the reserved `.metadata`.
    ReservedFirstComponent,
    /// Path contains a NUL byte.
    ContainsNul,
    /// Path uses a backslash separator (forward slashes only).
    BackslashSeparator,
    /// First component equals the canonical category (producer rule).
    StartsWithCategory,
    /// More than `MAX_PATH_COMPONENTS` components.
    TooManyComponents,
    /// A component fails sanitization (becomes a different value).
    UnsanitaryComponent,
    /// Resolved path would exceed PATH_MAX.
    PathTooLong,
    /// Resolved path escapes `<library_root>/<category>/`.
    EscapesCatRoot,
}

/// A successfully parsed link tag, plus its slash-separated components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTag<'a> {
    pub raw: &'a str,
    pub relative_path: &'a str,
    pub components: Vec<&'a str>,
}

/// Parse + validate a `link:` tag against the rules in §5 that don't require
/// runtime context (i.e. no library_root / category needed).
///
/// `category` is optional — when supplied, the producer-side rule "first
/// component must not equal the canonical category" is enforced.
pub fn parse_link_tag<'a>(tag: &'a str, category: Option<&str>) -> Result<LinkTag<'a>, LinkTagError> {
    let rel = tag
        .strip_prefix(LINK_TAG_PREFIX)
        .ok_or(LinkTagError::MissingPrefix)?;

    if rel.is_empty() {
        return Err(LinkTagError::EmptyPath);
    }
    if rel.contains('\0') {
        return Err(LinkTagError::ContainsNul);
    }
    if rel.contains('\\') {
        return Err(LinkTagError::BackslashSeparator);
    }
    if rel.starts_with('/') {
        return Err(LinkTagError::AbsolutePath);
    }

    let components: Vec<&str> = rel.split('/').collect();
    if components.len() > MAX_PATH_COMPONENTS {
        return Err(LinkTagError::TooManyComponents);
    }
    for c in &components {
        if c.is_empty() || *c == "." || *c == ".." {
            return Err(LinkTagError::InvalidComponent);
        }
    }
    if components[0] == RESERVED_FIRST_COMPONENT {
        return Err(LinkTagError::ReservedFirstComponent);
    }
    if let Some(cat) = category {
        if components[0] == cat {
            return Err(LinkTagError::StartsWithCategory);
        }
    }

    Ok(LinkTag {
        raw: tag,
        relative_path: rel,
        components,
    })
}

/// Resolve a parsed link tag to its on-disk hardlink site (§5 Resolution).
///
/// Returns `<library_root>/<category>/<rel>/sanitize(name)`.
/// Verifies the result stays under `<library_root>/<category>/` (rule 7) and
/// fits within `PATH_MAX` (rule 8). Components inside `rel` are *also*
/// re-sanitized; if any change, returns `UnsanitaryComponent` so producers
/// know their script's output isn't byte-stable.
pub fn resolve_link_site(
    library_root: &std::path::Path,
    category: &str,
    tag: &LinkTag<'_>,
    torrent_name: &str,
    opts: &SanitizeOpts,
) -> Result<std::path::PathBuf, LinkTagError> {
    let cat_clean = sanitize_component(category, opts);
    if cat_clean != category {
        return Err(LinkTagError::UnsanitaryComponent);
    }

    let mut out = library_root.to_path_buf();
    out.push(&cat_clean);

    for c in &tag.components {
        let cleaned = sanitize_component(c, opts);
        if cleaned != *c {
            return Err(LinkTagError::UnsanitaryComponent);
        }
        out.push(&cleaned);
    }

    out.push(sanitize_component(torrent_name, opts));

    if out.as_os_str().len() > PATH_MAX {
        return Err(LinkTagError::PathTooLong);
    }

    // Containment check (rule 7): resolved path must stay under
    // <library_root>/<category>/. Lexical, since we have no symlinks at
    // this layer; symlink-aware resolution is the linker's job.
    let cat_root = library_root.join(&cat_clean);
    if !out.starts_with(&cat_root) {
        return Err(LinkTagError::EscapesCatRoot);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn opts() -> SanitizeOpts {
        SanitizeOpts::default()
    }

    // ---- sanitize_component ----

    #[test]
    fn sanitize_replaces_slash_and_nul() {
        assert_eq!(sanitize_component("a/b", &opts()), "a_b");
        assert_eq!(sanitize_component("a\0b", &opts()), "a_b");
    }

    #[test]
    fn sanitize_trims_whitespace() {
        assert_eq!(sanitize_component("  hello  ", &opts()), "hello");
    }

    #[test]
    fn sanitize_empty_becomes_underscore() {
        assert_eq!(sanitize_component("", &opts()), "_");
        assert_eq!(sanitize_component("   ", &opts()), "_");
    }

    #[test]
    fn sanitize_windows_chars() {
        assert_eq!(sanitize_component(r#"a<b>c:d"e|f?g*h\i"#, &opts()), "a_b_c_d_e_f_g_h_i");
    }

    #[test]
    fn sanitize_trailing_dot_or_space() {
        // Trailing dots survive the trim step, then get replaced.
        assert_eq!(sanitize_component("foo.", &opts()), "foo_");
        assert_eq!(sanitize_component("foo...", &opts()), "foo_");
        // Trailing whitespace is removed by the trim step (§10 step 2).
        assert_eq!(sanitize_component("foo ", &opts()), "foo");
    }

    #[test]
    fn sanitize_windows_reserved() {
        assert_eq!(sanitize_component("CON", &opts()), "_CON");
        assert_eq!(sanitize_component("nul.txt", &opts()), "_nul.txt");
        assert_eq!(sanitize_component("COM10", &opts()), "COM10"); // not reserved
    }

    #[test]
    fn sanitize_truncates_to_200_bytes() {
        let s = "a".repeat(500);
        let out = sanitize_component(&s, &opts());
        assert_eq!(out.len(), MAX_COMPONENT_BYTES);
    }

    #[test]
    fn sanitize_truncate_is_utf8_safe() {
        // Each ñ is 2 bytes; 150 of them = 300 bytes, truncated to 200 bytes
        // = 100 ñ's. The cut must land on a char boundary.
        let s: String = std::iter::repeat('ñ').take(150).collect();
        let out = sanitize_component(&s, &opts());
        assert!(out.is_char_boundary(out.len()));
        assert!(out.len() <= MAX_COMPONENT_BYTES);
    }

    #[test]
    fn sanitize_is_idempotent() {
        let cases = ["a/b", "  CON  ", "x*y", "....", "ñ/€", ""];
        for c in cases {
            let once = sanitize_component(c, &opts());
            let twice = sanitize_component(&once, &opts());
            assert_eq!(once, twice, "non-idempotent on {c:?}");
        }
    }

    // ---- parse_link_tag ----

    #[test]
    fn link_tag_happy_path() {
        let t = parse_link_tag("link:Computer/Internet/Hamza Farooq", None).unwrap();
        assert_eq!(t.relative_path, "Computer/Internet/Hamza Farooq");
        assert_eq!(t.components, vec!["Computer", "Internet", "Hamza Farooq"]);
    }

    #[test]
    fn link_tag_missing_prefix() {
        assert_eq!(parse_link_tag("Computer/x", None), Err(LinkTagError::MissingPrefix));
    }

    #[test]
    fn link_tag_empty() {
        assert_eq!(parse_link_tag("link:", None), Err(LinkTagError::EmptyPath));
    }

    #[test]
    fn link_tag_absolute() {
        assert_eq!(parse_link_tag("link:/x", None), Err(LinkTagError::AbsolutePath));
    }

    #[test]
    fn link_tag_dot_components() {
        assert_eq!(parse_link_tag("link:a/./b", None), Err(LinkTagError::InvalidComponent));
        assert_eq!(parse_link_tag("link:a/../b", None), Err(LinkTagError::InvalidComponent));
        assert_eq!(parse_link_tag("link:a//b", None), Err(LinkTagError::InvalidComponent));
    }

    #[test]
    fn link_tag_reserved_metadata() {
        assert_eq!(parse_link_tag("link:.metadata/x", None), Err(LinkTagError::ReservedFirstComponent));
    }

    #[test]
    fn link_tag_nul() {
        assert_eq!(parse_link_tag("link:a\0b", None), Err(LinkTagError::ContainsNul));
    }

    #[test]
    fn link_tag_backslash() {
        assert_eq!(parse_link_tag(r"link:a\b", None), Err(LinkTagError::BackslashSeparator));
    }

    #[test]
    fn link_tag_starts_with_category() {
        assert_eq!(
            parse_link_tag("link:myanonamouse.net/x", Some("myanonamouse.net")),
            Err(LinkTagError::StartsWithCategory)
        );
    }

    #[test]
    fn link_tag_too_many_components() {
        let path = std::iter::repeat("a").take(MAX_PATH_COMPONENTS + 1).collect::<Vec<_>>().join("/");
        let tag = format!("link:{path}");
        assert_eq!(parse_link_tag(&tag, None), Err(LinkTagError::TooManyComponents));
    }

    // ---- resolve_link_site ----

    #[test]
    fn resolve_basic() {
        let tag = parse_link_tag("link:Computer/Internet/Author", None).unwrap();
        let p = resolve_link_site(
            std::path::Path::new("/lib"),
            "myanonamouse.net",
            &tag,
            "Some Book",
            &opts(),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/lib/myanonamouse.net/Computer/Internet/Author/Some Book"));
    }

    #[test]
    fn resolve_unsanitary_component_rejected() {
        let tag = parse_link_tag("link:foo./bar", None).unwrap();
        let err = resolve_link_site(
            std::path::Path::new("/lib"),
            "cat",
            &tag,
            "n",
            &opts(),
        )
        .unwrap_err();
        assert_eq!(err, LinkTagError::UnsanitaryComponent);
    }

    // ---- properties ----

    proptest! {
        #[test]
        fn prop_sanitize_never_empty(s in ".*") {
            let out = sanitize_component(&s, &opts());
            prop_assert!(!out.is_empty());
        }

        #[test]
        fn prop_sanitize_idempotent(s in ".*") {
            let once = sanitize_component(&s, &opts());
            let twice = sanitize_component(&once, &opts());
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn prop_sanitize_no_forbidden_chars(s in ".*") {
            let out = sanitize_component(&s, &opts());
            for c in out.chars() {
                prop_assert!(c != '\0' && c != '/' && c != '\\');
                prop_assert!(!matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'));
            }
        }

        #[test]
        fn prop_sanitize_size_bounded(s in ".*") {
            let out = sanitize_component(&s, &opts());
            prop_assert!(out.len() <= MAX_COMPONENT_BYTES);
        }

        #[test]
        fn prop_resolved_path_contained(
            // build a plausible relative path from 1..=4 ascii components
            comps in proptest::collection::vec("[A-Za-z0-9]{1,8}", 1..=4)
        ) {
            let rel = comps.join("/");
            let raw = format!("link:{rel}");
            let tag = parse_link_tag(&raw, None).unwrap();
            let lib = std::path::PathBuf::from("/lib");
            let cat = "tracker.tld";
            let resolved = resolve_link_site(&lib, cat, &tag, "name", &opts()).unwrap();
            prop_assert!(resolved.starts_with(lib.join(cat)));
        }
    }
}
