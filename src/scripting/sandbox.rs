//! Rhai engine builder with the sandboxing rules from DESIGN.md §12.
//!
//! Pure: takes a [`SandboxLimits`] and returns a fresh `rhai::Engine` with
//! no I/O, no time, no random, no `eval` recursion. Only safe packages are
//! re-enabled on top of `Engine::new_raw()`.

use rhai::packages::{
    BasicArrayPackage, BasicMapPackage, BasicMathPackage, BasicStringPackage, LogicPackage, Package,
};
use rhai::Engine;

use crate::paths::{sanitize_component, SanitizeOpts};

/// Per-call resource caps. Mirrors the `[scripting]` config block.
#[derive(Debug, Clone, Copy)]
pub struct SandboxLimits {
    /// Max Rhai operations (statements + ops). Bounds CPU.
    pub max_operations: u64,
    /// Max bytes per string value the script can build.
    pub max_string_size: usize,
    /// Max elements per array.
    pub max_array_size: usize,
    /// Max entries per map.
    pub max_map_size: usize,
    /// Max function-call depth.
    pub max_call_levels: usize,
    /// Max `expr.expr.expr` chain depth, etc.
    pub max_expr_depth: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            max_operations: 200_000,
            max_string_size: 64 * 1024,
            max_array_size: 1024,
            max_map_size: 1024,
            max_call_levels: 32,
            max_expr_depth: 64,
        }
    }
}

/// Build a fresh sandboxed engine.
///
/// Always returns the same shape: no host fns are stateful, so callers may
/// reuse an engine across `classify` invocations on different scripts.
pub fn build_engine(limits: &SandboxLimits) -> Engine {
    let mut engine = Engine::new_raw();

    // Re-enable only the pure, sandbox-safe packages.
    engine.register_global_module(BasicArrayPackage::new().as_shared_module());
    engine.register_global_module(BasicStringPackage::new().as_shared_module());
    engine.register_global_module(BasicMapPackage::new().as_shared_module());
    engine.register_global_module(BasicMathPackage::new().as_shared_module());
    engine.register_global_module(LogicPackage::new().as_shared_module());

    engine.set_max_operations(limits.max_operations);
    engine.set_max_string_size(limits.max_string_size);
    engine.set_max_array_size(limits.max_array_size);
    engine.set_max_map_size(limits.max_map_size);
    engine.set_max_call_levels(limits.max_call_levels);
    engine.set_max_expr_depths(limits.max_expr_depth, limits.max_expr_depth);

    // `eval` is a built-in keyword even on `Engine::new_raw()`; disable it
    // explicitly so scripts can't escape the AST that was sandboxed at
    // compile time. I/O / time / random are not in the basic packages.
    engine.disable_symbol("eval");

    register_host_fns(&mut engine);

    engine
}

/// Pure host helpers exposed to scripts (§12 "Host-provided helpers").
fn register_host_fns(engine: &mut Engine) {
    // sanitize(component) — applies §10 to one path component.
    engine.register_fn("sanitize", |s: &str| -> String {
        sanitize_component(s, &SanitizeOpts::default())
    });

    // slug(s) — opinionated slugger: NFKD-fold, replace any char not in
    // [A-Za-z0-9._-] with `_`, collapse runs.
    engine.register_fn("slug", |s: &str| -> String { slug(s) });
}

fn slug(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkd: String = s.nfkd().collect();
    let mut out = String::with_capacity(nfkd.len());
    let mut last_underscore = false;
    for c in nfkd.chars() {
        let keep = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if keep {
            out.push(c);
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "_".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_has_no_print_capture_required() {
        // sanity: building the engine doesn't panic and basic eval works.
        let engine = build_engine(&SandboxLimits::default());
        let v: i64 = engine.eval("40 + 2").unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn engine_max_operations_enforced() {
        let limits = SandboxLimits {
            max_operations: 100,
            ..SandboxLimits::default()
        };
        let engine = build_engine(&limits);
        let res = engine.eval::<i64>(
            r#"
            let n = 0;
            for i in 0..10000 { n += 1; }
            n
            "#,
        );
        assert!(res.is_err(), "expected operation budget to fire");
    }

    #[test]
    fn engine_no_filesystem_access() {
        // `open_file` and friends are not registered on `Engine::new_raw`.
        let engine = build_engine(&SandboxLimits::default());
        let res = engine.eval::<()>("open_file(\"/etc/passwd\")");
        assert!(res.is_err());
    }

    #[test]
    fn engine_no_eval_recursion() {
        let engine = build_engine(&SandboxLimits::default());
        let res = engine.eval::<i64>(r#"eval("1+1")"#);
        assert!(res.is_err(), "eval must not be available in sandbox");
    }

    #[test]
    fn host_sanitize_helper() {
        let engine = build_engine(&SandboxLimits::default());
        let s: String = engine.eval(r#"sanitize("a/b")"#).unwrap();
        assert_eq!(s, "a_b");
    }

    #[test]
    fn host_slug_helper() {
        let engine = build_engine(&SandboxLimits::default());
        let s: String = engine.eval(r#"slug("Hello, World!")"#).unwrap();
        assert_eq!(s, "Hello_World");
    }

    #[test]
    fn slug_basic_cases() {
        assert_eq!(slug("Hello World"), "Hello_World");
        assert_eq!(slug("a/b\\c"), "a_b_c");
        assert_eq!(slug("---"), "---"); // dash is allowed
        assert_eq!(slug("!!!"), "_");
    }
}
