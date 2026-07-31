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

fn calls<'a>(graph: &'a Value, source: &str, target: &str) -> Vec<&'a Value> {
    graph["edges"]
        .as_array()
        .expect("graph edges should be an array")
        .iter()
        .filter(|edge| {
            edge["kind"] == "calls" && edge["source"] == source && edge["target"] == target
        })
        .collect()
}

#[test]
fn attributes_initializer_and_file_scope_calls_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("calls.ts"),
        r#"function helper() {}

function outer() {
  const label = helper();
}

const bootstrap = helper();

class Worker {
  handler = () => helper();
}

helper();
"#,
    )
    .unwrap();

    let graph = analyze(dir.path());
    let outer_id = node_id(&graph, "outer", "function");
    let helper_id = node_id(&graph, "helper", "function");
    let file_id = node_id(&graph, "calls.ts", "file");

    let outer_calls = calls(&graph, &outer_id, &helper_id);
    assert_eq!(
        outer_calls.len(),
        1,
        "outer initializer call should be retained"
    );
    assert_eq!(
        outer_calls[0]["provenance"][0]["span"]["start"][0], 3,
        "initializer call should keep its original source span"
    );
    assert_eq!(
        outer_calls[0]["provenance"][0]["symbol_id"], outer_id,
        "initializer call provenance should identify its callable owner"
    );

    let file_calls = calls(&graph, &file_id, &helper_id);
    assert_eq!(
        file_calls.len(),
        1,
        "equivalent file-scoped calls should merge into one edge"
    );
    let provenance = file_calls[0]["provenance"]
        .as_array()
        .expect("merged call should retain provenance");
    assert_eq!(
        provenance.len(),
        3,
        "top-level initializer, class function field, and script call should each be recorded once"
    );
    let mut rows = provenance
        .iter()
        .map(|entry| {
            assert_eq!(
                entry["symbol_id"], file_id,
                "file-scoped call provenance should identify the file node"
            );
            entry["span"]["start"][0]
                .as_u64()
                .expect("call provenance should include a source row")
        })
        .collect::<Vec<_>>();
    rows.sort_unstable();
    assert_eq!(rows, vec![6, 9, 12]);
}
