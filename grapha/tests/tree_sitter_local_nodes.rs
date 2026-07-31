use assert_cmd::Command;
use serde_json::Value;

fn grapha() -> Command {
    Command::cargo_bin("grapha").unwrap()
}

fn analyze(path: &std::path::Path) -> Value {
    let output = grapha()
        .args(["analyze", path.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).expect("analyze output should be JSON")
}

fn node_id(graph: &Value, name: &str, kind: &str) -> String {
    graph["nodes"]
        .as_array()
        .expect("graph nodes should be an array")
        .iter()
        .find(|node| node["name"] == name && node["kind"] == kind)
        .unwrap_or_else(|| panic!("{kind} node {name} should exist"))["id"]
        .as_str()
        .expect("node id should be a string")
        .to_string()
}

fn has_node(graph: &Value, name: &str, kind: &str) -> bool {
    graph["nodes"]
        .as_array()
        .expect("graph nodes should be an array")
        .iter()
        .any(|node| node["name"] == name && node["kind"] == kind)
}

fn has_call(graph: &Value, source: &str, target: &str) -> bool {
    graph["edges"]
        .as_array()
        .expect("graph edges should be an array")
        .iter()
        .any(|edge| edge["kind"] == "calls" && edge["source"] == source && edge["target"] == target)
}

#[test]
fn generic_extractor_omits_ordinary_locals_and_keeps_initializer_calls() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("scope.ts"),
        r#"
function helper() {}

function work() {
  const localConstant = helper();
  let localVariable = helper();
  const localHandler = () => helper();
}

const moduleConstant = helper();
let moduleVariable = helper();
"#,
    )
    .unwrap();

    let graph = analyze(dir.path());
    let helper_id = node_id(&graph, "helper", "function");
    let work_id = node_id(&graph, "work", "function");
    let file_id = node_id(&graph, "scope.ts", "file");
    let local_handler_id = node_id(&graph, "localHandler", "function");

    assert!(
        !has_node(&graph, "localConstant", "constant"),
        "ordinary constants declared in a callable must not become graph nodes"
    );
    assert!(
        !has_node(&graph, "localVariable", "variable"),
        "ordinary variables declared in a callable must not become graph nodes"
    );
    assert!(
        has_node(&graph, "moduleConstant", "constant"),
        "file-scoped constants must remain graph nodes"
    );
    assert!(
        has_node(&graph, "moduleVariable", "variable"),
        "file-scoped variables must remain graph nodes"
    );
    assert!(
        has_call(&graph, &work_id, &helper_id),
        "an ordinary local initializer call must remain attributed to its callable"
    );
    assert!(
        has_call(&graph, &file_id, &helper_id),
        "a file-scoped initializer call must remain attributed to the file"
    );
    assert!(
        has_call(&graph, &local_handler_id, &helper_id),
        "function-valued local declarations must retain function call attribution"
    );
}
