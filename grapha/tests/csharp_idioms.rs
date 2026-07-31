use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn grapha() -> Command {
    Command::cargo_bin("grapha").unwrap()
}

fn analyze(path: &Path) -> Value {
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

#[test]
fn extracts_csharp_enum_members_and_record_method_containment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Domain.cs"),
        r#"
public enum JobState
{
    Queued,
    Running = 2,
}

public record Job(string Id)
{
    public string Display()
    {
        return Id;
    }
}
"#,
    )
    .unwrap();

    let graph = analyze(dir.path());
    let job_id = node_id(&graph, "Job", "class");
    let display_id = node_id(&graph, "Display", "function");

    node_id(&graph, "Queued", "variant");
    node_id(&graph, "Running", "variant");
    assert!(
        graph["edges"]
            .as_array()
            .expect("graph edges should be an array")
            .iter()
            .any(|edge| {
                edge["kind"] == "contains"
                    && edge["source"] == job_id
                    && edge["target"] == display_id
            }),
        "record should contain its declared method"
    );
}
