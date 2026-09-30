//! Function extraction via tree-sitter (python, go, c) — deterministic,
//! source-preserving spans; never calls a model.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    Go,
    C,
}

pub struct FnSpan {
    /// 1-based inclusive line numbers.
    pub start: u64,
    pub end: u64,
    pub body: String,
}

pub fn lang_for_path(p: &Path) -> Option<Lang> {
    let ext = p.extension()?.to_str()?;
    match ext {
        "py" => Some(Lang::Python),
        "go" => Some(Lang::Go),
        "c" | "h" => Some(Lang::C),
        _ => None,
    }
}

pub fn extract_functions(content: &str, lang: Lang) -> Vec<FnSpan> {
    let kinds: &[&str] = match lang {
        Lang::Python => &["function_definition"],
        Lang::Go => &["function_declaration", "method_declaration"],
        Lang::C => &["function_definition"],
    };

    let mut parser = tree_sitter::Parser::new();
    let language = match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
    };
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if kinds.contains(&node.kind()) {
            let body = node.utf8_text(bytes).unwrap_or("").to_string();
            out.push(FnSpan {
                start: node.start_position().row as u64 + 1,
                end: node.end_position().row as u64 + 1,
                body,
            });
            continue; // functions don't nest for these grammars' outer spans
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    out.sort_by_key(|f| f.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_functions() {
        let src = "import os\n\ndef greet(name):\n    return f\"hi {name}\"\n\nclass A:\n    def m(self):\n        return 1\n";
        let fns = extract_functions(src, Lang::Python);
        assert_eq!(fns.len(), 2);
        assert_eq!(fns[0].start, 3);
        assert_eq!(fns[0].end, 4);
        assert!(fns[0].body.contains("return f\"hi {name}\""));
        assert_eq!(fns[1].start, 7);
    }

    #[test]
    fn go_functions() {
        let src = "package main\n\nfunc main() {\n\tprintln(1)\n}\n\nfunc (s S) M() int {\n\treturn 2\n}\n";
        let fns = extract_functions(src, Lang::Go);
        assert_eq!(fns.len(), 2);
        assert!(fns[0].body.contains("println(1)"));
        assert!(fns[1].body.contains("return 2"));
    }

    #[test]
    fn c_functions() {
        let src = "#include <stdio.h>\n\nint add(int a, int b) {\n    return a + b;\n}\n";
        let fns = extract_functions(src, Lang::C);
        assert_eq!(fns.len(), 1);
        assert!(fns[0].body.contains("return a + b;"));
    }
}
