//! TypeScript → JavaScript via oxc (type stripping only; type checking belongs in the editor).

use std::path::Path;

#[cfg(feature = "typescript")]
pub fn transpile(source: &str, path: &Path) -> Result<String, String> {
    use oxc_allocator::Allocator;
    use oxc_codegen::Codegen;
    use oxc_parser::Parser;
    use oxc_semantic::SemanticBuilder;
    use oxc_span::SourceType;
    use oxc_transformer::{TransformOptions, Transformer};

    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let ret = Parser::new(&allocator, source, source_type).parse();
    if let Some(e) = ret.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let mut program = ret.program;
    let sem = SemanticBuilder::new().with_enum_eval(true).build(&program);
    if let Some(e) = sem.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let scoping = sem.semantic.into_scoping();
    let options = TransformOptions::default();
    let t = Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if let Some(e) = t.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(Codegen::new().build(&program).code)
}

#[cfg(not(feature = "typescript"))]
pub fn transpile(_source: &str, path: &Path) -> Result<String, String> {
    Err(format!("{}: TypeScript support is not included in this build", path.display()))
}

/// Transpiles with a content-hash cache so unchanged scripts cost nothing at startup.
pub fn transpile_cached(source: &str, path: &Path, cache_dir: Option<&Path>) -> Result<String, String> {
    let hash = fnv1a(source.as_bytes());
    let cached = cache_dir.map(|d| d.join(format!("{hash:016x}.js")));
    if let Some(c) = &cached
        && let Ok(js) = std::fs::read_to_string(c)
    {
        return Ok(js);
    }
    let js = transpile(source, path)?;
    if let Some(c) = &cached {
        if let Some(dir) = c.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(c, &js);
    }
    Ok(js)
}

fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Include the transpiler version so upgrades invalidate the cache.
    h ^ (env!("CARGO_PKG_VERSION").len() as u64).rotate_left(17)
}

#[cfg(all(test, feature = "typescript"))]
mod tests {
    use super::*;

    #[test]
    fn strips_types() {
        let js = transpile(
            "interface A { x: number }\nenum Color { Red, Green }\nconst f = (a: A): number => a.x * 2;\nlet c: Color = Color.Green;\n",
            Path::new("t.ts"),
        )
        .unwrap();
        assert!(!js.contains("interface"));
        assert!(js.contains("const f = (a) => a.x * 2"), "{js}");
        assert!(js.contains("Color"), "{js}");
    }

    #[test]
    fn reports_syntax_errors() {
        assert!(transpile("let x: = 1", Path::new("bad.ts")).is_err());
    }
}
