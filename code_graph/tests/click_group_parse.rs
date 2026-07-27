use code_graph::snooper::lang::python;

#[test]
fn click_core_indexes_group_and_command() {
    let Some(path) = code_graph::resolve_optional_test_repo("click/src/click/core.py") else {
        return;
    };
    if !path.is_file() {
        return;
    }
    let src = std::fs::read_to_string(&path).unwrap();
    let parsed = python::parser::parse(path, &src).unwrap();
    let names: Vec<_> = parsed
        .blocks
        .iter()
        .filter(|b| matches!(b.name.as_str(), "Group" | "Command" | "Context" | "Option"))
        .map(|b| (b.name.clone(), b.kind.clone(), b.start_line))
        .collect();
    println!("hits: {:?}", names);
    assert!(names.iter().any(|(n, k, _)| n == "Command" && k.contains("class")), "{:?}", names);
    assert!(names.iter().any(|(n, k, _)| n == "Group" && k.contains("class")), "Group missing: {:?}", names);
}
