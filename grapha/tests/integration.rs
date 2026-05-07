use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;

fn grapha() -> Command {
    Command::cargo_bin("grapha").unwrap()
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn contains_file_named(root: &Path, file_name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            return true;
        }
        if path.is_dir() && contains_file_named(&path, file_name) {
            return true;
        }
    }
    false
}

fn strip_ansi(input: &str) -> String {
    let mut stripped = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        stripped.push(ch);
    }
    stripped
}

#[test]
fn analyzes_single_file() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"struct\""))
        .stdout(predicate::str::contains("\"name\": \"Config\""))
        .stdout(predicate::str::contains("\"kind\": \"function\""))
        .stdout(predicate::str::contains("\"name\": \"default_config\""));
}

#[test]
fn analyzes_directory() {
    grapha()
        .args(["analyze", "tests/fixtures/multi"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"run\""))
        .stdout(predicate::str::contains("\"name\": \"helper\""));
}

#[test]
fn filter_option_works() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.rs", "--filter", "fn"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"function\""))
        .stdout(predicate::str::contains("\"kind\": \"struct\"").not());
}

#[test]
fn output_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.json");

    grapha()
        .args([
            "analyze",
            "tests/fixtures/simple.rs",
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    let content = std::fs::read_to_string(&output).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["version"], "0.1.0");
    assert!(!parsed["nodes"].as_array().unwrap().is_empty());
}

#[test]
fn empty_directory_produces_empty_graph() {
    let dir = tempfile::tempdir().unwrap();
    grapha()
        .args(["analyze", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"nodes\": []"));
}

#[test]
fn analyzes_swift_file() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.swift"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"struct\""))
        .stdout(predicate::str::contains("\"name\": \"Config\""))
        .stdout(predicate::str::contains("\"kind\": \"function\""));
}

#[test]
fn invalid_filter_shows_error() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.rs", "--filter", "bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown node kind"));
}

#[test]
fn compact_flag_produces_grouped_output() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.rs", "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"files\""))
        .stdout(predicate::str::contains("\"symbols\""))
        .stdout(predicate::str::contains("\"span\""));
}

#[test]
fn doc_comments_drive_search_concepts_and_compact_output() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("gift.rs"),
        "/// Coordinates the gift flow handoff.\npub struct CheckoutCoordinator;\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let search_output = grapha()
        .args([
            "symbol",
            "search",
            "gift flow",
            "-p",
            dir.path().to_str().unwrap(),
            "--fields",
            "id,doc_comment",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&search_output).unwrap();
    assert_eq!(parsed[0]["name"], "CheckoutCoordinator");
    assert!(
        parsed[0]["doc_comment"]
            .as_str()
            .is_some_and(|doc| doc.contains("Coordinates the gift flow handoff."))
    );

    grapha()
        .args([
            "concept",
            "search",
            "gift flow",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
            "--fields",
            "doc_comment",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[doc_comment] gift flow"))
        .stdout(predicate::str::contains(
            "/// Coordinates the gift flow handoff.",
        ));

    grapha()
        .args(["analyze", dir.path().to_str().unwrap(), "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"doc_comment\": \"/// Coordinates the gift flow handoff.\\n\"",
        ));
}

#[test]
fn symbol_annotations_round_trip_through_cli_context_and_concept_search() {
    let dir = tempfile::tempdir().unwrap();
    let grapha_home = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("gift.rs"),
        "pub struct CheckoutCoordinator;\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let annotation_text = "Owns the gift handoff between catalog and checkout.";
    grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "symbol",
            "annotate",
            "CheckoutCoordinator",
            annotation_text,
            "-p",
            dir.path().to_str().unwrap(),
            "--by",
            "codex",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(annotation_text))
        .stdout(predicate::str::contains("\"stale\": false"));

    let context_output = grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "symbol",
            "context",
            "CheckoutCoordinator",
            "-p",
            dir.path().to_str().unwrap(),
            "--fields",
            "annotation",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&context_output).unwrap();
    assert_eq!(parsed["symbol"]["annotation"]["text"], annotation_text);
    assert_eq!(parsed["symbol"]["annotation"]["created_by"], "codex");

    let concept_output = grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "concept",
            "search",
            "gift handoff",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&concept_output).unwrap();
    assert!(
        parsed["scopes"][0]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|evidence| evidence["kind"] == "annotation"
                && evidence["source_value"]
                    .as_str()
                    .is_some_and(|value| value.contains("gift handoff"))),
        "concept search should report annotation evidence: {parsed:#?}"
    );

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
            "--full-rebuild",
        ])
        .assert()
        .success();

    grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "symbol",
            "annotation",
            "CheckoutCoordinator",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(annotation_text));
}

#[test]
fn symbol_annotations_are_shared_across_git_worktrees_but_indexes_stay_local() {
    let dir = tempfile::tempdir().unwrap();
    let grapha_home = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    let linked = dir.path().join("linked");
    std::fs::create_dir(&main).unwrap();
    std::fs::create_dir(main.join("src")).unwrap();
    std::fs::write(
        main.join("src").join("lib.rs"),
        "pub struct CheckoutCoordinator;\n",
    )
    .unwrap();

    run_git(&main, &["init"]);
    run_git(&main, &["add", "src/lib.rs"]);
    run_git(
        &main,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test User",
            "commit",
            "-m",
            "init",
        ],
    );
    run_git(&main, &["worktree", "add", linked.to_str().unwrap()]);

    grapha()
        .args(["index", main.to_str().unwrap()])
        .assert()
        .success();
    grapha()
        .args(["index", linked.to_str().unwrap()])
        .assert()
        .success();

    let annotation_text = "Shared worktree knowledge for checkout coordination.";
    grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "symbol",
            "annotate",
            "CheckoutCoordinator",
            annotation_text,
            "-p",
            main.to_str().unwrap(),
            "--by",
            "codex",
        ])
        .assert()
        .success();

    grapha()
        .env("GRAPHA_HOME", grapha_home.path())
        .args([
            "symbol",
            "annotation",
            "CheckoutCoordinator",
            "-p",
            linked.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(annotation_text));

    assert!(main.join(".grapha").join("grapha.db").exists());
    assert!(linked.join(".grapha").join("grapha.db").exists());
    assert!(contains_file_named(grapha_home.path(), "annotations.db"));
    assert!(!contains_file_named(grapha_home.path(), "grapha.db"));
    assert!(!contains_file_named(grapha_home.path(), "search_index"));
}

#[test]
fn compact_flag_preserves_swiftui_hierarchy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import SwiftUI

        struct Row: View {
            let title: String
            var body: some View { Text(title) }
        }

        struct ContentView: View {
            var body: some View {
                VStack {
                    Text("Hello")
                    Row(title: "World")
                }
            }
        }
        "#,
    )
    .unwrap();

    grapha()
        .args(["analyze", dir.path().to_str().unwrap(), "--compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"view\""))
        .stdout(predicate::str::contains("\"name\": \"body\""))
        .stdout(predicate::str::contains("\"members\": ["))
        .stdout(predicate::str::contains("\"VStack\""))
        .stdout(predicate::str::contains("\"Text\""))
        .stdout(predicate::str::contains("\"Row\""))
        .stdout(predicate::str::contains("\"type_refs\": ["));
}

#[test]
fn output_contains_version() {
    grapha()
        .args(["analyze", "tests/fixtures/simple.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"version\": \"0.1.0\""));
}

fn write_localizable_fixture(path: &std::path::Path, key: &str, value: &str, comment: &str) {
    std::fs::write(
        path,
        format!(
            r#"{{
  "sourceLanguage" : "en",
  "strings" : {{
    "{key}" : {{
      "comment" : "{comment}",
      "localizations" : {{
        "en" : {{
          "stringUnit" : {{
            "state" : "translated",
            "value" : "{value}"
          }}
        }}
      }}
    }}
  }},
  "version" : "1.0"
}}"#
        ),
    )
    .unwrap();
}

fn write_strings_fixture(path: &std::path::Path, key: &str, value: &str) {
    std::fs::write(path, format!(r#""{key}" = "{value}";"#)).unwrap();
}

fn write_repo_smells_scope_fixture(dir: &std::path::Path) {
    std::fs::write(
        dir.join("main.rs"),
        r#"
        mod other;

        fn hot() {
            helper01();
            helper02();
            helper03();
            helper04();
            helper05();
            helper06();
            helper07();
            helper08();
            helper09();
            helper10();
            helper11();
            helper12();
            helper13();
            helper14();
            helper15();
            helper16();
        }

        fn helper01() {}
        fn helper02() {}
        fn helper03() {}
        fn helper04() {}
        fn helper05() {}
        fn helper06() {}
        fn helper07() {}
        fn helper08() {}
        fn helper09() {}
        fn helper10() {}
        fn helper11() {}
        fn helper12() {}
        fn helper13() {}
        fn helper14() {}
        fn helper15() {}
        fn helper16() {}
        "#,
    )
    .unwrap();

    std::fs::write(
        dir.join("other.rs"),
        r#"
        pub fn noisy() {
            other01();
            other02();
            other03();
            other04();
            other05();
            other06();
            other07();
            other08();
            other09();
            other10();
            other11();
            other12();
            other13();
            other14();
            other15();
            other16();
        }

        fn other01() {}
        fn other02() {}
        fn other03() {}
        fn other04() {}
        fn other05() {}
        fn other06() {}
        fn other07() {}
        fn other08() {}
        fn other09() {}
        fn other10() {}
        fn other11() {}
        fn other12() {}
        fn other13() {}
        fn other14() {}
        fn other15() {}
        fn other16() {}
        "#,
    )
    .unwrap();
}

#[test]
fn cli_smoke_matrix_help_contracts() {
    let cases = [
        (
            vec!["analyze", "--help"],
            "Analyze source files and output graph",
        ),
        (
            vec!["index", "--help"],
            "Build or refresh the persistent Grapha store",
        ),
        (
            vec!["symbol", "--help"],
            "Query symbol relationships and search indexed symbols",
        ),
        (
            vec!["flow", "--help"],
            "Inspect dataflow between symbols, entries, and effects",
        ),
        (
            vec!["l10n", "--help"],
            "Inspect localization references and usage sites",
        ),
        (
            vec!["asset", "--help"],
            "Inspect image asset catalogs and usage sites",
        ),
        (
            vec!["repo", "--help"],
            "Run repository-scoped analysis over the indexed graph",
        ),
        (
            vec!["serve", "--mcp", "--help"],
            "Serve an indexed project either as an HTTP graph explorer",
        ),
    ];

    for (args, expected) in cases {
        grapha()
            .args(&args)
            .assert()
            .success()
            .stdout(predicate::str::contains(expected));
    }
}

#[test]
fn rich_help_examples_are_rendered() {
    grapha()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Grapha indexes source into a symbol graph",
        ))
        .stdout(predicate::str::contains("Typical workflow:"))
        .stdout(predicate::str::contains("grapha serve --mcp --watch"));

    grapha()
        .args(["index", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Build or refresh the persistent Grapha store",
        ))
        .stdout(predicate::str::contains("grapha index . --timing"));

    grapha()
        .args(["symbol", "search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Search the indexed graph"))
        .stdout(predicate::str::contains("ProfileAPI --repo FrameUI"));

    grapha()
        .args(["flow", "trace", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "reverse traces start at a symbol or terminal",
        ))
        .stdout(predicate::str::contains(
            "grapha flow trace sendGift --direction reverse",
        ));

    grapha()
        .args(["repo", "smells", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Scan the indexed graph for structural code smells",
        ))
        .stdout(predicate::str::contains("grapha repo smells --module Room"));
}

#[test]
fn serve_mcp_help_mentions_stdio_contract() {
    grapha()
        .args(["serve", "--mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Run as MCP server over stdio (instead of HTTP)",
        ))
        .stdout(predicate::str::contains("--mcp"));
}

#[test]
fn index_creates_sqlite_db() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    grapha()
        .args([
            "index",
            "tests/fixtures/simple.rs",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("indexed"));

    assert!(store_dir.join("grapha.db").exists());
    assert!(store_dir.join("localization.json").exists());
}

#[test]
fn index_json_format() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    grapha()
        .args([
            "index",
            "tests/fixtures/simple.rs",
            "--format",
            "json",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(store_dir.join("graph.json").exists());
    assert!(store_dir.join("localization.json").exists());
}

#[test]
fn index_reuses_cached_extractions_when_sources_are_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "mod helper;\nfn main() { helper::run(); }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("helper.rs"), "pub fn run() {}\n").unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "reused 2 cached extraction results",
        ))
        .stderr(predicate::str::contains("extracted 0 files"));
}

#[test]
fn repo_smells_file_scope_limits_results_to_matching_file() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    write_repo_smells_scope_fixture(dir.path());

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "repo",
            "smells",
            "--file",
            "main.rs",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"hot\""))
        .stdout(predicate::str::contains("\"name\": \"noisy\"").not());
}

#[test]
fn repo_smells_brief_format_works() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    write_repo_smells_scope_fixture(dir.path());

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "repo",
            "smells",
            "--file",
            "main.rs",
            "--format",
            "brief",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("smells: total="))
        .stdout(predicate::str::contains("hot [function]"))
        .stdout(predicate::str::contains("\"smells\"").not());
}

#[test]
fn repo_smells_symbol_scope_limits_results_to_symbol_neighborhood() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    write_repo_smells_scope_fixture(dir.path());

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "repo",
            "smells",
            "--symbol",
            "main.rs::hot",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"hot\""))
        .stdout(predicate::str::contains("\"name\": \"noisy\"").not());
}

#[test]
fn repo_infer_brief_saves_opt_in_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    let feature_dir = dir.path().join("Features/Gifts");
    std::fs::create_dir_all(&feature_dir).unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        "[inferred]\nenabled = true\n",
    )
    .unwrap();
    std::fs::write(
        feature_dir.join("run.rs"),
        "/// Starts the gift flow.\npub fn run() {}\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "repo",
            "infer",
            "--format",
            "brief",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "inferred: enabled=true saved=true total=",
        ))
        .stdout(predicate::str::contains("doc_code_link=1"))
        .stdout(predicate::str::contains("ownership=1"))
        .stdout(predicate::str::contains("Starts the gift flow."));

    let inferred = std::fs::read_to_string(store_dir.join("inferred.json")).unwrap();
    assert!(inferred.contains("\"kind\": \"doc_code_link\""));
    assert!(inferred.contains("\"confidence\": 0.7"));
}

#[test]
fn repo_doctor_brief_reports_stale_inferred_links() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    std::fs::write(
        store_dir.join("inferred.json"),
        r#"{
  "version": "1",
  "records": [
    {
      "id": "symbol:missing:doc",
      "kind": "doc_code_link",
      "target": {
        "kind": "symbol",
        "id": "missing"
      },
      "value": "old note",
      "confidence": 0.7,
      "source": "heuristic"
    }
  ]
}"#,
    )
    .unwrap();

    grapha()
        .args([
            "repo",
            "doctor",
            "--format",
            "brief",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("doctor: total=1 warning=1"))
        .stdout(predicate::str::contains("stale_inferred_link"))
        .stdout(predicate::str::contains("missing"));
}

#[test]
fn repo_smells_populates_graph_and_query_caches() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    write_repo_smells_scope_fixture(dir.path());

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let graph_cache_path = store_dir.join("graph.bincode");
    let query_cache_path = store_dir.join("query_cache.bin");
    assert!(!graph_cache_path.exists());
    assert!(!query_cache_path.exists());

    grapha()
        .args([
            "repo",
            "smells",
            "--file",
            "main.rs",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"hot\""));

    assert!(graph_cache_path.exists());
    assert!(query_cache_path.exists());
}

#[test]
fn repo_smells_no_cache_bypasses_graph_and_query_caches() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    write_repo_smells_scope_fixture(dir.path());

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let graph_cache_path = store_dir.join("graph.bincode");
    let query_cache_path = store_dir.join("query_cache.bin");
    assert!(!graph_cache_path.exists());
    assert!(!query_cache_path.exists());

    grapha()
        .args([
            "repo",
            "smells",
            "--file",
            "main.rs",
            "--no-cache",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"hot\""));

    assert!(!graph_cache_path.exists());
    assert!(!query_cache_path.exists());
}

#[test]
fn repo_arch_reports_configured_layer_violations() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "mod infra;\nmod ui;\nfn main() { infra::load(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("infra.rs"),
        "pub fn load() { crate::ui::render(); }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("ui.rs"), "pub fn render() {}\n").unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        r#"
[[architecture.layers]]
name = "ui"
patterns = ["ui.rs"]

[[architecture.layers]]
name = "infra"
patterns = ["infra.rs"]

[[architecture.deny]]
from = "infra"
to = "ui"
reason = "Infrastructure must not depend on UI."
"#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = grapha()
        .args(["repo", "arch", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(parsed["configured"], true);
    assert_eq!(parsed["total_violations"], 1);
    assert_eq!(parsed["violations"][0]["source_layer"], "infra");
    assert_eq!(parsed["violations"][0]["target_layer"], "ui");
    assert_eq!(
        parsed["violations"][0]["reason"],
        "Infrastructure must not depend on UI."
    );
}

#[test]
fn symbol_search_includes_id_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        r#"
        fn helper() {}
        fn run() { helper(); }
        "#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = grapha()
        .args([
            "symbol",
            "search",
            "helper",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let first = parsed.as_array().unwrap().first().unwrap();
    assert_eq!(first["name"], "helper");
    assert!(
        first.get("id").is_some(),
        "default search output should include id"
    );
}

#[test]
fn flow_entries_tree_respects_file_field_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        r#"
        fn helper() {}
        fn main() { helper(); }
        "#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "flow",
            "entries",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
            "--fields",
            "none",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("main [function]"))
        .stdout(predicate::str::contains("(main.rs)").not());
}

#[test]
fn flow_entries_file_scope_and_limit_returns_focused_subset() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("RoomPage.rs"),
        r#"
        pub fn room_body() {}
        pub fn room_share() {}
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ChatPage.rs"),
        r#"
        pub fn chat_body() {}
        "#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "flow",
            "entries",
            "-p",
            dir.path().to_str().unwrap(),
            "--file",
            "RoomPage.rs",
            "--limit",
            "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total\": 2"))
        .stdout(predicate::str::contains("\"shown\": 1"))
        .stdout(predicate::str::contains("RoomPage.rs"));
}

#[test]
fn flow_entries_file_filter_rejects_partial_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("RoomPage.rs"),
        r#"
        pub fn room_page() {}
        "#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "flow",
            "entries",
            "-p",
            dir.path().to_str().unwrap(),
            "--file",
            "Page",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total\": 0"))
        .stdout(predicate::str::contains("\"shown\": 0"));
}

#[test]
fn flow_origin_help_mentions_full_field_alias() {
    grapha()
        .args(["flow", "origin", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"full\"/\"all\"/\"none\""));
}

#[test]
fn index_skips_invalid_xcstrings_catalogs() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import SwiftUI

        struct ContentView: View {
            var body: some View { Text("Hello") }
        }
        "#,
    )
    .unwrap();
    write_localizable_fixture(
        &dir.path().join("Localizable.xcstrings"),
        "hello",
        "Hello",
        "Greeting",
    );
    std::fs::write(
        dir.path().join("Broken.xcstrings"),
        r#"{
  "sourceLanguage" : "en",
  "strings" : {
    "broken" : {},
  },
  "version" : "1.0"
}"#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "skipped invalid localization catalog Broken.xcstrings",
        ));

    assert!(store_dir.join("localization.json").exists());
}

#[test]
fn localize_and_usages_commands_resolve_swiftui_xcstrings() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import SwiftUI

        struct ContentView: View {
            var body: some View {
                VStack {
                    Text(.accountForgetPassword)
                }
            }
        }
        "#,
    )
    .unwrap();

    std::fs::write(
        dir.path().join("Strings.generated.swift"),
        r#"
        import Foundation

        public enum L10n {
            public static var accountForgetPassword: String {
                L10n.tr("Localizable", "account_forget_password", fallback: "Forgot Password")
            }

            private static func tr(_ table: String, _ key: String, fallback: String) -> String {
                fallback
            }
        }
        "#,
    )
    .unwrap();

    write_localizable_fixture(
        &dir.path().join("Localizable.xcstrings"),
        "account_forget_password",
        "Forgot Password",
        "Shown on the login screen",
    );

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(store_dir.join("localization.json").exists());
    std::fs::remove_file(dir.path().join("Localizable.xcstrings")).unwrap();

    let localize_output = grapha()
        .args(["l10n", "symbol", "body", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let localize: Value = serde_json::from_slice(&localize_output).unwrap();
    let matches = localize["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0]["record"]["key"].as_str(),
        Some("account_forget_password")
    );
    assert_eq!(
        matches[0]["record"]["catalog_file"].as_str(),
        Some("Localizable.xcstrings")
    );
    assert_eq!(
        matches[0]["record"]["source_value"].as_str(),
        Some("Forgot Password")
    );
    assert_eq!(
        matches[0]["reference"]["wrapper_name"].as_str(),
        Some("accountForgetPassword")
    );

    let usages_output = grapha()
        .args([
            "l10n",
            "usages",
            "account_forget_password",
            "--table",
            "Localizable",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let usages: Value = serde_json::from_slice(&usages_output).unwrap();
    let usage_records = usages["records"].as_array().unwrap();
    assert_eq!(usage_records.len(), 1);
    let usage_sites = usage_records[0]["usages"].as_array().unwrap();
    assert_eq!(usage_sites.len(), 1);
    assert_eq!(usage_sites[0]["owner"]["name"].as_str(), Some("body"));
    assert_eq!(usage_sites[0]["view"]["name"].as_str(), Some("Text"));
    assert_eq!(
        usage_sites[0]["reference"]["wrapper_name"].as_str(),
        Some("accountForgetPassword")
    );
}

#[test]
fn localize_and_usages_commands_resolve_swiftui_strings_with_l10n_resource() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    let resources_dir = dir.path().join("Resources/en.lproj");
    std::fs::create_dir_all(&resources_dir).unwrap();

    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import SwiftUI

        struct ContentView: View {
            var body: some View {
                VStack {
                    Text(i18n: .accountForgetPassword)
                }
            }
        }
        "#,
    )
    .unwrap();

    std::fs::write(
        dir.path().join("L10nResource.swift"),
        r#"
        import SwiftUI

        public struct L10nResource {
            public let key: String
            public let table: String
            public let fallback: String

            public init(_ key: String, table: String, fallback: String) {
                self.key = key
                self.table = table
                self.fallback = fallback
            }

            public var translation: String {
                fallback
            }
        }

        extension Text {
            public init(i18n resource: L10nResource) {
                self.init(resource.translation)
            }
        }
        "#,
    )
    .unwrap();

    std::fs::write(
        dir.path().join("Strings.generated.swift"),
        r#"
        import Foundation

        extension L10nResource {
            public static let accountForgetPassword = L10nResource(
                "account_forget_password",
                table: "Localizable",
                fallback: "Forgot Password"
            )
        }
        "#,
    )
    .unwrap();

    write_strings_fixture(
        &resources_dir.join("Localizable.strings"),
        "account_forget_password",
        "Forgot Password",
    );

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let localize_output = grapha()
        .args(["l10n", "symbol", "body", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let localize: Value = serde_json::from_slice(&localize_output).unwrap();
    let matches = localize["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0]["record"]["key"].as_str(),
        Some("account_forget_password")
    );
    assert_eq!(
        matches[0]["record"]["catalog_file"].as_str(),
        Some("Resources/en.lproj/Localizable.strings")
    );
    assert_eq!(
        matches[0]["reference"]["wrapper_base"].as_str(),
        Some("L10nResource")
    );

    let usages_output = grapha()
        .args([
            "l10n",
            "usages",
            "account_forget_password",
            "--table",
            "Localizable",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let usages: Value = serde_json::from_slice(&usages_output).unwrap();
    let usage_records = usages["records"].as_array().unwrap();
    assert_eq!(usage_records.len(), 1);
    assert_eq!(
        usage_records[0]["record"]["catalog_dir"].as_str(),
        Some("Resources")
    );
    assert_eq!(
        usage_records[0]["usages"][0]["reference"]["wrapper_base"].as_str(),
        Some("L10nResource")
    );
}

#[test]
fn usages_command_resolves_non_view_constructor_localization_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import Foundation

        public enum L10n {
            public static var roomShareDesc: String {
                L10n.tr("Localizable", "room_share_desc", fallback: "I'm in this room")
            }

            private static func tr(_ table: String, _ key: String, fallback: String) -> String {
                fallback
            }
        }

        struct ShareWithFriendsEntity {
            let shareText: String
            let shareLink: String
        }

        struct ContentView {
            func onShare(shareLink: String) {
                let entity = ShareWithFriendsEntity(
                    shareText: L10n.roomShareDesc,
                    shareLink: shareLink
                )
                _ = entity
            }
        }
        "#,
    )
    .unwrap();

    write_localizable_fixture(
        &dir.path().join("Localizable.xcstrings"),
        "room_share_desc",
        "I'm in this room",
        "Share prompt",
    );

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let usages_output = grapha()
        .args([
            "l10n",
            "usages",
            "room_share_desc",
            "--table",
            "Localizable",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let usages: Value = serde_json::from_slice(&usages_output).unwrap();
    let usage_records = usages["records"].as_array().unwrap();
    assert_eq!(usage_records.len(), 1);
    let usage_sites = usage_records[0]["usages"].as_array().unwrap();
    assert_eq!(usage_sites.len(), 1);
    assert_eq!(usage_sites[0]["owner"]["name"].as_str(), Some("onShare"));
    assert_eq!(usage_sites[0]["view"]["name"].as_str(), Some("shareText"));
    assert_eq!(
        usage_sites[0]["reference"]["wrapper_name"].as_str(),
        Some("roomShareDesc")
    );
}

#[test]
fn localize_and_usages_prefer_nearest_duplicate_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    let auth_sources = dir.path().join("Packages/Auth/Sources/Auth");
    let profile_sources = dir.path().join("Packages/Profile/Sources/Profile");
    std::fs::create_dir_all(&auth_sources).unwrap();
    std::fs::create_dir_all(&profile_sources).unwrap();

    std::fs::write(
        auth_sources.join("AuthView.swift"),
        r#"
        import SwiftUI

        struct AuthView: View {
            var body: some View {
                VStack {
                    Text(.sharedTitle)
                }
            }
        }
        "#,
    )
    .unwrap();

    std::fs::write(
        auth_sources.join("Strings.generated.swift"),
        r#"
        import Foundation

        public enum L10n {
            public static var sharedTitle: String {
                L10n.tr("Localizable", "shared_title", fallback: "Shared")
            }

            private static func tr(_ table: String, _ key: String, fallback: String) -> String {
                fallback
            }
        }
        "#,
    )
    .unwrap();

    write_localizable_fixture(
        &dir.path().join("Packages/Auth/Localizable.xcstrings"),
        "shared_title",
        "Auth Shared",
        "Auth catalog",
    );
    write_localizable_fixture(
        &dir.path().join("Packages/Profile/Localizable.xcstrings"),
        "shared_title",
        "Profile Shared",
        "Profile catalog",
    );

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let localize_output = grapha()
        .args([
            "l10n",
            "symbol",
            "AuthView",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let localize: Value = serde_json::from_slice(&localize_output).unwrap();
    let matches = localize["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0]["record"]["catalog_file"].as_str(),
        Some("Packages/Auth/Localizable.xcstrings")
    );
    assert_eq!(
        matches[0]["record"]["source_value"].as_str(),
        Some("Auth Shared")
    );

    let usages_output = grapha()
        .args([
            "l10n",
            "usages",
            "shared_title",
            "--table",
            "Localizable",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let usages: Value = serde_json::from_slice(&usages_output).unwrap();
    let usage_records = usages["records"].as_array().unwrap();
    assert_eq!(usage_records.len(), 2);

    let auth_record = usage_records
        .iter()
        .find(|record| {
            record["record"]["catalog_file"].as_str() == Some("Packages/Auth/Localizable.xcstrings")
        })
        .expect("auth catalog should be present");
    assert_eq!(auth_record["usages"].as_array().unwrap().len(), 1);
    assert_eq!(
        auth_record["usages"][0]["owner"]["file"].as_str(),
        Some("Packages/Auth/Sources/Auth/AuthView.swift")
    );

    let profile_record = usage_records
        .iter()
        .find(|record| {
            record["record"]["catalog_file"].as_str()
                == Some("Packages/Profile/Localizable.xcstrings")
        })
        .expect("profile catalog should be present");
    assert!(
        profile_record["usages"].as_array().unwrap().is_empty(),
        "farther duplicate catalog should not claim the AuthView usage"
    );
}

#[test]
fn repeated_index_uses_incremental_store_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    let source_path = dir.path().join("main.rs");
    std::fs::write(
        &source_path,
        "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("full_rebuild"));

    std::fs::write(
        &source_path,
        "pub fn gamma() {}\npub fn beta() { gamma(); }\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("incremental"));

    grapha()
        .args([
            "symbol",
            "search",
            "gamma",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"gamma\""))
        .stdout(predicate::str::contains("\"name\": \"alpha\"").not());
}

#[test]
fn dataflow_command_outputs_json_and_tree() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "pub fn handler() { persist(); }\nfn persist() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        r#"
[[classifiers]]
pattern = "persist"
terminal = "persistence"
direction = "read_write"
operation = "UPSERT"
"#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "flow",
            "graph",
            "handler",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"effect\""))
        .stdout(predicate::str::contains("\"kind\": \"read\""))
        .stdout(predicate::str::contains("\"kind\": \"write\""));

    grapha()
        .args([
            "flow",
            "graph",
            "handler",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("summary: symbols="))
        .stdout(predicate::str::contains("[effect:persistence]"))
        .stdout(predicate::str::contains("read ->"));
}

#[test]
fn tree_output_respects_color_modes() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "pub fn handler() { persist(); }\nfn persist() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        r#"
[[classifiers]]
pattern = "persist"
terminal = "persistence"
direction = "read_write"
operation = "UPSERT"
"#,
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let plain = grapha()
        .args([
            "flow",
            "graph",
            "handler",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
            "--color",
            "never",
        ])
        .output()
        .unwrap();
    assert!(plain.status.success());
    let plain_stdout = String::from_utf8(plain.stdout).unwrap();
    assert!(!plain_stdout.contains("\x1b["));

    let colored = grapha()
        .args([
            "flow",
            "graph",
            "handler",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
            "--color",
            "always",
        ])
        .output()
        .unwrap();
    assert!(colored.status.success());
    let colored_stdout = String::from_utf8(colored.stdout).unwrap();
    assert!(colored_stdout.contains("\x1b["));
    assert_eq!(strip_ansi(&colored_stdout), plain_stdout);
}

#[test]
fn json_output_ignores_color_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    grapha()
        .args([
            "index",
            "tests/fixtures/simple.rs",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = grapha()
        .args([
            "symbol",
            "context",
            "default_config",
            "-p",
            dir.path().to_str().unwrap(),
            "--color",
            "always",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("\x1b["));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["symbol"]["name"], "default_config");
}

#[test]
fn context_command_returns_symbol_info() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    // First, index
    grapha()
        .args([
            "index",
            "tests/fixtures/simple.rs",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Then query context
    grapha()
        .args([
            "symbol",
            "context",
            "default_config",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"default_config\""));
}

#[test]
fn search_fields_projection_works() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = grapha()
        .args([
            "symbol",
            "search",
            "main",
            "-p",
            dir.path().to_str().unwrap(),
            "--fields",
            "id,signature,role",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let first = &parsed[0];
    assert_eq!(first["name"], "main");
    assert_eq!(first["kind"], "function");
    assert_eq!(first["id"], "main.rs::main");
    assert_eq!(first["signature"], "fn main()");
    assert_eq!(first["role"], "entry_point");
    assert!(first.get("file").is_none());
}

#[test]
fn search_context_projection_keeps_relationships() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = grapha()
        .args([
            "symbol",
            "search",
            "main",
            "-p",
            dir.path().to_str().unwrap(),
            "--context",
            "--fields",
            "snippet",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    let first = &parsed[0];
    assert_eq!(first["name"], "main");
    assert!(first["snippet"].as_str().unwrap().contains("helper"));
    assert_eq!(first["calls"][0], "main.rs::helper");
}

#[test]
fn changes_command_runs_on_clean_repo() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    // Initialize a git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Configure git user for commits
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Create a Rust file and commit it
    std::fs::write(dir.path().join("main.rs"), "pub fn hello() {}").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Index it
    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Run changes — should succeed with no changes
    grapha()
        .args(["repo", "changes", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"changed_count\": 0"));
}

#[test]
fn repo_history_add_and_list_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().to_str().unwrap();

    let add_output = grapha()
        .args([
            "repo",
            "history",
            "add",
            "--kind",
            "test",
            "--title",
            "cargo test",
            "--at",
            "2026-04-24T10:00:00Z",
            "--status",
            "passed",
            "--file",
            "src/lib.rs",
            "--module",
            "core",
            "--meta",
            "duration_ms=1200",
            "-p",
            project,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let added: Value = serde_json::from_slice(&add_output).unwrap();
    assert_eq!(added["kind"], "test");
    assert_eq!(added["files"][0], "src/lib.rs");
    assert_eq!(added["metadata"]["duration_ms"], "1200");

    let list_output = grapha()
        .args([
            "repo", "history", "list", "--kind", "test", "--file", "lib.rs", "-p", project,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed: Value = serde_json::from_slice(&list_output).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["title"], "cargo test");
}

#[test]
fn search_command_finds_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    grapha()
        .args([
            "index",
            "tests/fixtures/simple.rs",
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    grapha()
        .args([
            "symbol",
            "search",
            "Config",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Config"));
}
