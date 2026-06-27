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

fn lacks_call(graph: &Value, source_name: &str, target_name: &str) -> bool {
    !has_call(graph, source_name, target_name)
}

fn has_entry_role(graph: &Value, name: &str) -> bool {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == name && node["role"]["type"] == "entry_point")
}

fn has_module(graph: &Value, name: &str, module: &str) -> bool {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == name && node["module"] == module)
}

fn has_call_operation(graph: &Value, source_name: &str, operation: &str) -> bool {
    let source_id = node_id(graph, source_name);
    graph["edges"].as_array().unwrap().iter().any(|edge| {
        edge["kind"] == "calls" && edge["source"] == source_id && edge["operation"] == operation
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

#[test]
fn indexes_android_kotlin_and_java_constructs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("settings.gradle"), "include ':app'\n").unwrap();
    std::fs::write(dir.path().join("build.gradle"), "").unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/build.gradle"), "").unwrap();
    let kotlin_dir = dir.path().join("app/src/main/java/com/example/game");
    std::fs::create_dir_all(&kotlin_dir).unwrap();
    std::fs::write(
        kotlin_dir.join("MainActivity.kt"),
        r#"
            package com.example.game

            import androidx.appcompat.app.AppCompatActivity

            val topLevelToken = "game"

            fun bootstrap() {
                topLevelToken.toString()
            }

            class MainActivity : AppCompatActivity() {
                val viewModel = GameViewModel()

                constructor(name: String) : this() {
                    render()
                }

                fun onCreate() {
                    render()
                }

                private fun render() {
                    bootstrap()
                    SessionStore.load()
                    dynamicLinkPrefix.isNullOrEmpty()
                }

                companion object {
                    const val TAG = "MainActivity"

                    fun launch() {
                        bootstrap()
                    }
                }
            }

            object SessionStore {
                fun load() {}
            }

            object QMUILangHelper {
                fun isNullOrEmpty(value: String): Boolean = value.isEmpty()
            }

            interface GameRepository {
                fun games(): List<String>
            }

            enum class GameState {
                Waiting,
                Running
            }

            typealias PlayerId = String
        "#,
    )
    .unwrap();
    std::fs::write(
        kotlin_dir.join("JavaActivity.java"),
        r#"
            package com.example.game;

            import android.app.Activity;
            import okhttp3.OkHttpClient;

            public class JavaActivity extends Activity {
                private String title;

                public JavaActivity() {
                    onStart();
                }

                public void onStart() {
                    helper();
                    startActivity(null);
                    getSharedPreferences("prefs", 0).edit().putString("id", "1");
                    new OkHttpClient().newCall(null).enqueue(null);
                }

                private void helper() {}
            }

            public record PlayerRecord(String id) {
                public PlayerRecord {
                    validate(id);
                }
            }

            enum JavaState {
                READY,
                DONE
            }
        "#,
    )
    .unwrap();

    let graph = analyze(dir.path());

    assert!(has_node(&graph, "MainActivity", "class"));
    assert!(has_node(&graph, "GameRepository", "trait"));
    assert!(has_node(&graph, "GameState", "enum"));
    assert!(has_node(&graph, "Waiting", "variant"));
    assert!(has_node(&graph, "SessionStore", "class"));
    assert!(has_node(&graph, "Companion", "class"));
    assert!(has_node(&graph, "constructor", "function"));
    assert!(has_node(&graph, "onCreate", "function"));
    assert!(has_node(&graph, "render", "function"));
    assert!(has_node(&graph, "launch", "function"));
    assert!(has_node(&graph, "load", "function"));
    assert!(has_node(&graph, "bootstrap", "function"));
    assert!(has_node(&graph, "topLevelToken", "variable"));
    assert!(has_node(&graph, "viewModel", "field"));
    assert!(has_node(&graph, "TAG", "field"));
    assert!(has_node(&graph, "PlayerId", "type_alias"));
    assert!(has_node(&graph, "JavaActivity", "class"));
    assert!(has_node(&graph, "PlayerRecord", "class"));
    assert!(has_node(&graph, "JavaState", "enum"));
    assert!(has_node(&graph, "READY", "variant"));
    assert!(has_call(&graph, "onCreate", "render"));
    assert!(has_call(&graph, "render", "bootstrap"));
    assert!(has_call(&graph, "render", "load"));
    assert!(lacks_call(&graph, "render", "isNullOrEmpty"));
    assert!(has_call(&graph, "onStart", "helper"));
    assert!(has_entry_role(&graph, "MainActivity"));
    assert!(has_entry_role(&graph, "onCreate"));
    assert!(has_entry_role(&graph, "JavaActivity"));
    assert!(has_entry_role(&graph, "onStart"));
    assert!(has_module(&graph, "MainActivity", "app"));
    assert!(has_call_operation(&graph, "onStart", "http"));
    assert!(has_call_operation(&graph, "onStart", "storage"));
    assert!(has_call_operation(&graph, "onStart", "event"));
}
