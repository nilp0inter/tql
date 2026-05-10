//! Run a tracker's `classify(input)` script and validate its output.
//!
//! Pure with respect to the host: takes a Rhai `Engine`, a script source
//! string, and an `input` Map; returns a validated [`ClassifyOutput`].
//! No filesystem I/O here — the registry layer (Leg 9) handles disk loads.

use rhai::{Dynamic, Engine, Map, Scope, AST};

use crate::paths::{
    parse_link_tag, SOFT_MAX_LINK_TAGS, SOFT_MAX_LINK_TAG_BYTES,
};
use crate::scripting::types::{ClassifyError, ClassifyOutput};

/// Hard cap on the number of info tags a script can emit. Belt-and-braces
/// against pathological scripts; the engine's `max_array_size` is the
/// primary defense.
pub const MAX_INFO_TAGS: usize = 64;
/// Hard cap on the number of warnings.
pub const MAX_WARNINGS: usize = 32;
/// Hard cap on link tags (above the §5 soft cap, which is warn-only).
pub const MAX_LINK_TAGS_HARD: usize = 64;

/// Compile a script source for repeated runs. Surfaces compile errors as
/// [`ClassifyError::Compile`].
pub fn compile(engine: &Engine, source: &str) -> Result<AST, ClassifyError> {
    engine
        .compile(source)
        .map_err(|e| ClassifyError::Compile(e.to_string()))
}

/// Run a pre-compiled script's `classify(input)` and validate its output
/// against §5's producer rules.
///
/// `canonical_category` is the manifest's `canonical_category`; producer
/// rule "no link tag's path begins with the canonical category" is enforced
/// against it.
///
/// Soft caps (§5) — link tag byte length and number of link tags — are
/// folded into `output.warnings`. Hard caps (this module's constants)
/// return [`ClassifyError::TooLarge`].
pub fn run_classify(
    engine: &Engine,
    ast: &AST,
    input: Map,
    canonical_category: &str,
) -> Result<ClassifyOutput, ClassifyError> {
    let mut scope = Scope::new();
    let result: Dynamic = engine
        .call_fn(&mut scope, ast, "classify", (input,))
        .map_err(|e| ClassifyError::Runtime(e.to_string()))?;

    let map = result
        .try_cast::<Map>()
        .ok_or_else(|| ClassifyError::BadReturnShape("classify must return an object map".into()))?;

    let link_tags = take_string_array(&map, "link_tags")?;
    let info_tags = take_string_array(&map, "info_tags")?;
    let mut warnings = take_string_array(&map, "warnings")?;

    if link_tags.len() > MAX_LINK_TAGS_HARD {
        return Err(ClassifyError::TooLarge(format!(
            "link_tags has {} entries, hard cap is {}",
            link_tags.len(),
            MAX_LINK_TAGS_HARD
        )));
    }
    if info_tags.len() > MAX_INFO_TAGS {
        return Err(ClassifyError::TooLarge(format!(
            "info_tags has {} entries, hard cap is {}",
            info_tags.len(),
            MAX_INFO_TAGS
        )));
    }
    if warnings.len() > MAX_WARNINGS {
        return Err(ClassifyError::TooLarge(format!(
            "warnings has {} entries, hard cap is {}",
            warnings.len(),
            MAX_WARNINGS
        )));
    }

    if link_tags.is_empty() {
        return Err(ClassifyError::NoLinkTags);
    }

    // §5 producer rules. Each tag is parsed with the canonical_category as
    // the "first component must not equal" check.
    for tag in &link_tags {
        if let Err(reason) = parse_link_tag(tag, Some(canonical_category)) {
            return Err(ClassifyError::InvalidLinkTag {
                tag: tag.clone(),
                reason,
            });
        }
    }

    // Soft caps — warn, don't fail.
    if link_tags.len() > SOFT_MAX_LINK_TAGS {
        warnings.push(format!(
            "soft cap: {} link tags (>{}), inspect script",
            link_tags.len(),
            SOFT_MAX_LINK_TAGS
        ));
    }
    for t in &link_tags {
        if t.len() > SOFT_MAX_LINK_TAG_BYTES {
            warnings.push(format!(
                "soft cap: link tag {} bytes (>{})",
                t.len(),
                SOFT_MAX_LINK_TAG_BYTES
            ));
        }
    }

    Ok(ClassifyOutput {
        link_tags,
        info_tags,
        warnings,
    })
}

fn take_string_array(map: &Map, key: &str) -> Result<Vec<String>, ClassifyError> {
    let v = map.get(key).cloned().ok_or_else(|| {
        ClassifyError::BadReturnShape(format!("missing field `{key}` in classify output"))
    })?;
    let arr = v.try_cast::<rhai::Array>().ok_or_else(|| {
        ClassifyError::BadReturnShape(format!("`{key}` must be an array"))
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.into_iter().enumerate() {
        let s = item.into_immutable_string().map_err(|t| {
            ClassifyError::BadReturnShape(format!(
                "`{key}[{i}]` must be a string, got {t}"
            ))
        })?;
        out.push(s.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::sandbox::{build_engine, SandboxLimits};

    fn engine() -> Engine {
        build_engine(&SandboxLimits::default())
    }

    fn input_map(pairs: &[(&str, Dynamic)]) -> Map {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).into(), v.clone());
        }
        m
    }

    const HAPPY: &str = r#"
        fn classify(input) {
            let link_tags = [];
            for cat in input.categories {
                link_tags.push("link:" + cat);
            }
            #{
                link_tags: link_tags,
                info_tags: ["lang:" + input.language],
                warnings: [],
            }
        }
    "#;

    #[test]
    fn classify_happy_path() {
        let eng = engine();
        let ast = compile(&eng, HAPPY).unwrap();
        let cats: rhai::Array = vec![Dynamic::from("Computer/Internet".to_string())];
        let input = input_map(&[
            ("categories", Dynamic::from(cats)),
            ("language", Dynamic::from("English".to_string())),
        ]);
        let out = run_classify(&eng, &ast, input, "myanonamouse.net").unwrap();
        assert_eq!(out.link_tags, vec!["link:Computer/Internet".to_string()]);
        assert_eq!(out.info_tags, vec!["lang:English".to_string()]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn classify_must_return_a_map() {
        let eng = engine();
        let src = r#"fn classify(input) { 42 }"#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        assert!(matches!(err, ClassifyError::BadReturnShape(_)));
    }

    #[test]
    fn classify_missing_field() {
        let eng = engine();
        let src = r#"fn classify(input) { #{ link_tags: ["link:x"], info_tags: [] } }"#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        match err {
            ClassifyError::BadReturnShape(m) => assert!(m.contains("warnings"), "{m}"),
            other => panic!("expected BadReturnShape, got {other:?}"),
        }
    }

    #[test]
    fn classify_array_element_not_string() {
        let eng = engine();
        let src = r#"
            fn classify(input) {
                #{ link_tags: ["link:x", 42], info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        assert!(matches!(err, ClassifyError::BadReturnShape(_)));
    }

    #[test]
    fn classify_no_link_tags() {
        let eng = engine();
        let src = r#"
            fn classify(input) {
                #{ link_tags: [], info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        assert!(matches!(err, ClassifyError::NoLinkTags));
    }

    #[test]
    fn classify_rejects_tag_starting_with_category() {
        let eng = engine();
        let src = r#"
            fn classify(input) {
                #{ link_tags: ["link:myanonamouse.net/x"], info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "myanonamouse.net").unwrap_err();
        assert!(matches!(
            err,
            ClassifyError::InvalidLinkTag { reason: crate::paths::LinkTagError::StartsWithCategory, .. }
        ));
    }

    #[test]
    fn classify_rejects_missing_prefix() {
        let eng = engine();
        let src = r#"
            fn classify(input) {
                #{ link_tags: ["Computer/x"], info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        assert!(matches!(
            err,
            ClassifyError::InvalidLinkTag { reason: crate::paths::LinkTagError::MissingPrefix, .. }
        ));
    }

    #[test]
    fn classify_compile_error() {
        let eng = engine();
        let err = compile(&eng, "fn classify(input) { this is not rhai }").unwrap_err();
        assert!(matches!(err, ClassifyError::Compile(_)));
    }

    #[test]
    fn classify_runtime_op_budget() {
        let eng = build_engine(&SandboxLimits { max_operations: 200, ..Default::default() });
        let src = r#"
            fn classify(input) {
                let n = 0;
                for i in 0..100000 { n += 1; }
                #{ link_tags: ["link:x"], info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let err = run_classify(&eng, &ast, Map::new(), "cat").unwrap_err();
        assert!(matches!(err, ClassifyError::Runtime(_)));
    }

    #[test]
    fn classify_soft_cap_warns_on_long_tag() {
        let eng = engine();
        let long = "a".repeat(SOFT_MAX_LINK_TAG_BYTES); // tag = "link:" + long → > soft cap
        let src = format!(
            r#"fn classify(input) {{ #{{ link_tags: ["link:{long}"], info_tags: [], warnings: [] }} }}"#
        );
        let ast = compile(&eng, &src).unwrap();
        let out = run_classify(&eng, &ast, Map::new(), "cat").unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("soft cap")));
    }
}
