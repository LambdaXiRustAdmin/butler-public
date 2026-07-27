//! Shared Tree-sitter walks for C and C++ (prototype extraction + name unwrapping).
//! Grammar-specific node kinds that only exist in one language are handled gracefully
//! (missing kinds simply never match).

use crate::BlockInfo;
use std::collections::HashSet;
use std::path::PathBuf;
use tree_sitter::Node;

/// Emit free-function prototypes as `function_declaration` with the given lang tag.
pub fn collect_function_prototypes(
    root: Node,
    file: &PathBuf,
    source: &str,
    lang: &str,
    blocks: &mut Vec<BlockInfo>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "declaration" && declaration_is_function_prototype(&node) {
            if let Some(name) = extract_name_from_declarator_chain(&node, source) {
                if !name.is_empty() {
                    let start_line = node.start_position().row + 1;
                    let end_line = node.end_position().row + 1;
                    let start_byte = node.start_byte();
                    let end_byte = node.end_byte();
                    let block = BlockInfo::new(
                        file.clone(),
                        "function_declaration",
                        lang,
                        start_line,
                        end_line,
                        start_byte,
                        end_byte,
                        source[start_byte..end_byte].to_string(),
                        &name,
                        HashSet::new(),
                    );
                    if !blocks.iter().any(|b| b.id == block.id) {
                        blocks.push(block);
                    }
                }
            }
            continue;
        }
        if node.kind() == "function_definition" {
            continue;
        }
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            stack.push(ch);
        }
    }
}

fn declaration_is_function_prototype(node: &Node) -> bool {
    if node.kind() != "declaration" {
        return false;
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if declarator_chain_is_function(&ch) {
            return true;
        }
    }
    false
}

fn declarator_chain_is_function(n: &Node) -> bool {
    match n.kind() {
        "function_declarator" => true,
        "pointer_declarator"
        | "reference_declarator"
        | "parenthesized_declarator"
        | "array_declarator"
        | "init_declarator"
        | "attributed_declarator" => n
            .child_by_field_name("declarator")
            .map(|d| declarator_chain_is_function(&d))
            .unwrap_or(false),
        _ => false,
    }
}

fn extract_name_from_declarator_chain(node: &Node, source: &str) -> Option<String> {
    if let Some(decl) = node.child_by_field_name("declarator") {
        return unwrap_declarator_name(&decl, source);
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if matches!(
            ch.kind(),
            "function_declarator"
                | "pointer_declarator"
                | "reference_declarator"
                | "parenthesized_declarator"
                | "init_declarator"
        ) {
            if let Some(n) = unwrap_declarator_name(&ch, source) {
                return Some(n);
            }
        }
    }
    None
}

pub fn unwrap_declarator_name(decl: &Node, source: &str) -> Option<String> {
    let mut decl = *decl;
    loop {
        match decl.kind() {
            "identifier" | "field_identifier" | "operator_name" | "destructor_name" => {
                return Some(source[decl.start_byte()..decl.end_byte()].to_string());
            }
            "pointer_declarator"
            | "reference_declarator"
            | "parenthesized_declarator"
            | "function_declarator"
            | "array_declarator"
            | "init_declarator"
            | "attributed_declarator" => {
                if let Some(child) = decl.child_by_field_name("declarator") {
                    decl = child;
                    continue;
                }
                let mut c = decl.walk();
                for ch in decl.children(&mut c) {
                    if matches!(
                        ch.kind(),
                        "identifier" | "field_identifier" | "operator_name" | "destructor_name"
                    ) {
                        return Some(source[ch.start_byte()..ch.end_byte()].to_string());
                    }
                }
                return None;
            }
            _ => {
                let mut c = decl.walk();
                for ch in decl.children(&mut c) {
                    if matches!(
                        ch.kind(),
                        "identifier" | "field_identifier" | "operator_name" | "destructor_name"
                    ) {
                        return Some(source[ch.start_byte()..ch.end_byte()].to_string());
                    }
                }
                return None;
            }
        }
    }
}

pub fn extract_name(node: &Node, source: &str) -> Option<String> {
    // Prefer grammar `name` field (class_specifier / struct_specifier / …).
    if let Some(name_node) = node.child_by_field_name("name") {
        let k = name_node.kind();
        if k == "identifier" || k == "type_identifier" || k == "field_identifier" {
            return Some(source[name_node.start_byte()..name_node.end_byte()].to_string());
        }
        // qualified_identifier etc. — take rightmost type_identifier
        if let Some(n) = deepest_type_or_id(&name_node, source) {
            return Some(n);
        }
    }
    if node.kind() == "function_definition" {
        if let Some(decl) = node.child_by_field_name("declarator") {
            return unwrap_declarator_name(&decl, source);
        }
    }
    // Do **not** walk all children for struct/class: inheritance clauses contain
    // `type_identifier`s (`struct Derived : public Base`) that must not become the name.
    if matches!(node.kind(), "struct_specifier" | "class_specifier" | "enum_specifier") {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k == "identifier"
            || k == "type_identifier"
            || k == "field_identifier"
            || k == "operator_name"
        {
            return Some(source[child.start_byte()..child.end_byte()].to_string());
        }
    }
    None
}

fn deepest_type_or_id(node: &Node, source: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    ) {
        return Some(source[node.start_byte()..node.end_byte()].to_string());
    }
    let mut cursor = node.walk();
    let mut last = None;
    for child in node.children(&mut cursor) {
        if let Some(n) = deepest_type_or_id(&child, source) {
            last = Some(n);
        }
    }
    last
}

/// **Problem class (universal C/C++):** tree-sitter-cpp misparses
/// `struct|class EXPORT_MACRO TypeName : public Base { … }` as `function_definition`
/// when an attribute/export macro sits where the grammar expects the type name.
/// Same pattern as `DLL_EXPORT`, `FOO_API`, `WINAPI`-style tokens — not one repo.
///
/// Recover kind + true type name so the warehouse indexes a type hub, not a fake fn.
pub fn recover_macro_mangled_type_blocks(blocks: &mut [crate::BlockInfo]) {
    for b in blocks.iter_mut() {
        if b.kind != "function_definition" {
            continue;
        }
        let Some((kind, name)) = classify_macro_mangled_type_def(&b.source) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        b.kind = kind.to_string();
        b.name = name;
        b.sig_hash = blake3::hash(b.name.as_bytes()).to_hex().to_string();
        b.id = crate::Id::new(&b.file, &b.kind, &b.content_hash);
    }
}

/// If source looks like `struct|class [MACROS…] TypeName … {` (or forward `;`), recover it.
fn classify_macro_mangled_type_def(source: &str) -> Option<(&'static str, String)> {
    let mut rest = source.trim_start();
    let kind = if let Some(r) = rest.strip_prefix("struct") {
        if !r.starts_with(|c: char| c.is_ascii_whitespace() || c == '/') {
            return None;
        }
        rest = r.trim_start();
        "struct_specifier"
    } else if let Some(r) = rest.strip_prefix("class") {
        if !r.starts_with(|c: char| c.is_ascii_whitespace() || c == '/') {
            return None;
        }
        rest = r.trim_start();
        "class_specifier"
    } else {
        return None;
    };

    // Skip line comments between keywords
    rest = skip_cpp_trivia(rest);

    // Skip SCREAMING_SNAKE export/attribute macros and optional (args).
    let mut name = None;
    while let Some((ident, after)) = take_cpp_ident(rest) {
        rest = after;
        rest = skip_cpp_trivia(rest);
        if is_screaming_macro_ident(ident) {
            if rest.starts_with('(') {
                rest = skip_balanced_parens(rest)?;
                rest = skip_cpp_trivia(rest);
            }
            continue;
        }
        name = Some(ident.to_string());
        break;
    }
    let name = name?;
    // Real type bodies / forwards — not `struct Foo foo_var = …` variable decls (rare as fn_def).
    let has_body = source.contains('{');
    let is_forward = source.trim_end().ends_with(';') && !has_body;
    if !has_body && !is_forward {
        return None;
    }
    // Heuristic: inheritance or body braces strongly signal a type definition.
    if has_body || rest.trim_start().starts_with(':') || is_forward {
        return Some((kind, name));
    }
    None
}

/// Attribute / linkage macros between `struct|class` and the type name.
/// Shape-based (SCREAMING_SNAKE / all-caps idents), not a denylist of projects.
fn is_screaming_macro_ident(s: &str) -> bool {
    if s.len() < 2 {
        return false;
    }
    let mut has_alpha = false;
    for c in s.chars() {
        if c.is_ascii_alphabetic() {
            has_alpha = true;
            if !c.is_ascii_uppercase() {
                return false;
            }
        } else if !(c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    has_alpha
}

fn take_cpp_ident(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = first.len_utf8();
    for (i, c) in chars {
        if c.is_ascii_alphanumeric() || c == '_' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    Some((&s[..end], &s[end..]))
}

fn skip_cpp_trivia(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        if s.starts_with("//") {
            if let Some(i) = s.find('\n') {
                s = s[i + 1..].trim_start();
                continue;
            }
            return "";
        }
        if s.starts_with("/*") {
            if let Some(i) = s.find("*/") {
                s = s[i + 2..].trim_start();
                continue;
            }
            return "";
        }
        break;
    }
    s
}

fn skip_balanced_parens(s: &str) -> Option<&str> {
    if !s.starts_with('(') {
        return Some(s);
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[i + 1..].trim_start());
                }
            }
            _ => {}
        }
    }
    None
}
