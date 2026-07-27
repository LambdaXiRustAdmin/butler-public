//! Optional canary: large C++ monorepos that use export macros between `struct` and the type
//! name. Skips if checkout missing. Logic under test is universal (see unit tests in cpp/parser).
use code_graph::snooper::lang::cpp;

#[test]
fn canary_export_macro_type_hub_indexes_as_struct() {
    let Some(path) = code_graph::resolve_optional_test_repo("pytorch/c10/core/TensorImpl.h") else {
        eprintln!("skip: optional canary checkout missing");
        return;
    };
    if !path.is_file() {
        eprintln!("skip: optional canary checkout missing");
        return;
    }
    let source = std::fs::read_to_string(&path).expect("read");
    let parsed = cpp::parser::parse(path.clone(), &source).expect("parse");
    let named: Vec<_> = parsed
        .blocks
        .iter()
        .filter(|b| b.name == "TensorImpl")
        .collect();
    assert!(!named.is_empty(), "expected TensorImpl blocks");
    let type_hubs: Vec<_> = named
        .iter()
        .filter(|b| {
            let k = b.kind.to_ascii_lowercase();
            (k.contains("struct") || k.contains("class")) && b.source.contains('{')
        })
        .collect();
    assert!(
        !type_hubs.is_empty(),
        "export-macro type must recover as struct/class hub; got: {:?}",
        named.iter().map(|b| (&b.kind, b.start_line)).collect::<Vec<_>>()
    );
    assert!(
        type_hubs
            .iter()
            .any(|b| b.end_line > b.start_line + 50),
        "type hub should be a large body"
    );
}

#[test]
fn canary_derived_header_does_not_steal_base_name() {
    let Some(path) =
        code_graph::resolve_optional_test_repo("pytorch/aten/src/ATen/NestedTensorImpl.h")
    else {
        return;
    };
    if !path.is_file() {
        return;
    }
    let source = std::fs::read_to_string(&path).expect("read");
    let parsed = cpp::parser::parse(path, &source).expect("parse");
    let wrong: Vec<_> = parsed
        .blocks
        .iter()
        .filter(|b| b.name == "TensorImpl")
        .map(|b| (b.kind.as_str(), b.start_line))
        .collect();
    assert!(
        wrong.is_empty(),
        "derived type must not index under base name; got {:?}",
        wrong
    );
}
