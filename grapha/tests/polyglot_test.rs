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

fn has_node(graph: &Value, name: &str, kind: &str) -> bool {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == name && node["kind"] == kind)
}

fn node_id(graph: &Value, name: &str) -> String {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == name)
        .unwrap_or_else(|| panic!("node {name} should exist"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn has_call(graph: &Value, source_name: &str, target_name: &str) -> bool {
    let source_id = node_id(graph, source_name);
    let target_id = node_id(graph, target_name);
    graph["edges"].as_array().unwrap().iter().any(|edge| {
        edge["kind"] == "calls" && edge["source"] == source_id && edge["target"] == target_id
    })
}

#[test]
fn smoke_analyzes_codegraph_tree_sitter_language_set() {
    let dir = tempfile::tempdir().unwrap();
    let samples = [
        (
            "main.ts",
            "export function tsMain(): void { tsHelper(); }\nfunction tsHelper(): void {}\n",
            "tsMain",
        ),
        (
            "component.tsx",
            "export function TsxView() { return <div />; }\n",
            "TsxView",
        ),
        (
            "main.js",
            "export function jsMain() { jsHelper(); }\nfunction jsHelper() {}\n",
            "jsMain",
        ),
        (
            "main.py",
            "class PyThing:\n    def py_main(self):\n        pass\n",
            "PyThing",
        ),
        (
            "main.go",
            "package main\nfunc goMain() { goHelper() }\nfunc goHelper() {}\n",
            "goMain",
        ),
        (
            "Main.java",
            "public class JavaThing { void javaMain() { javaHelper(); } void javaHelper() {} }\n",
            "JavaThing",
        ),
        (
            "main.c",
            "void c_helper() {}\nvoid c_main() { c_helper(); }\n",
            "c_main",
        ),
        (
            "main.cpp",
            "class CppThing { public: void cppMain() { cppHelper(); } void cppHelper() {} };\n",
            "CppThing",
        ),
        (
            "Program.cs",
            "public class CsThing { void CsMain() { CsHelper(); } void CsHelper() {} }\n",
            "CsThing",
        ),
        (
            "index.php",
            "<?php function phpMain() { phpHelper(); } function phpHelper() {} class PhpThing {}\n",
            "phpMain",
        ),
        (
            "app.rb",
            "class RubyThing\n  def ruby_main\n  end\nend\n",
            "RubyThing",
        ),
        (
            "Main.kt",
            "class KotlinThing {\n  fun kotlinMain() {\n    kotlinHelper()\n  }\n  fun kotlinHelper() {}\n}\n",
            "KotlinThing",
        ),
        (
            "main.dart",
            "class DartThing { void dartMain() { dartHelper(); } void dartHelper() {} }\n",
            "DartThing",
        ),
        (
            "UThing.pas",
            "unit UThing;\ninterface\ntype\n  TThing = class\n  end;\nimplementation\nend.\n",
            "TThing",
        ),
    ];

    for (file, source, _) in samples {
        std::fs::write(dir.path().join(file), source).unwrap();
    }

    let graph = analyze(dir.path());

    for (_, _, expected_name) in samples {
        assert!(
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["name"] == expected_name),
            "expected node {expected_name} in graph"
        );
    }
}

#[test]
fn analyzes_typescript_with_codegraph_style_constructs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("service.ts"),
        r#"
            export interface UserRepo {
              find(id: string): User;
            }

            import { User } from "./models";

            export class PaymentService {
              async charge(amount: number): number {
                return processPayment(amount);
              }
            }

            router.get("/pay", processPayment);

            export function processPayment(amount: number): number {
              return amount;
            }

            export const useAuth = () => {
              return getUser();
            };

            function getUser(): string {
              return "wendell";
            }

            let cache = 1;
        "#,
    )
    .unwrap();

    let graph = analyze(dir.path());

    assert!(has_node(&graph, "service.ts", "file"));
    assert!(has_node(&graph, "./models", "import"));
    assert!(has_node(&graph, "UserRepo", "trait"));
    assert!(has_node(&graph, "PaymentService", "class"));
    assert!(has_node(&graph, "charge", "function"));
    assert!(has_node(&graph, "processPayment", "function"));
    assert!(has_node(&graph, "useAuth", "function"));
    assert!(has_node(&graph, "cache", "variable"));
    assert!(has_node(&graph, "GET /pay", "route"));
    assert!(has_call(&graph, "charge", "processPayment"));
    assert!(has_call(&graph, "useAuth", "getUser"));
}

#[test]
fn extracts_react_component_and_next_route_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let pages = dir.path().join("pages");
    std::fs::create_dir_all(&pages).unwrap();
    std::fs::write(
        pages.join("dashboard.tsx"),
        r#"
            export default function Dashboard() {
              return <main />;
            }
        "#,
    )
    .unwrap();

    let graph = analyze(dir.path());

    assert!(has_node(&graph, "Dashboard", "component"));
    assert!(has_node(&graph, "/dashboard", "route"));
}

#[test]
fn analyzes_python_and_java_files_in_the_same_project() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("worker.py"),
        r#"
            import os

            class Worker:
                def run(self):
                    helper()

            def helper():
                return os.getcwd()
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("App.java"),
        r#"
            import java.util.List;

            public class App {
                void run() {
                    helper();
                }

                void helper() {}
            }
        "#,
    )
    .unwrap();

    let graph = analyze(dir.path());

    assert!(has_node(&graph, "Worker", "class"));
    assert!(has_node(&graph, "App", "class"));
    assert!(has_node(&graph, "run", "function"));
    assert!(has_node(&graph, "helper", "function"));
    assert!(has_call(&graph, "run", "helper"));
}
