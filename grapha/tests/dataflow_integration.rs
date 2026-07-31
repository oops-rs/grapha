use assert_cmd::Command;
use predicates::prelude::*;

fn grapha() -> Command {
    Command::cargo_bin("grapha").unwrap()
}

fn index_temp_project(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(dir.path().join("main.rs"), source).unwrap();

    grapha()
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--store-dir",
            store_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    dir
}

#[test]
fn analyze_outputs_dataflow_fields() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() { save_data(); }\nfn save_data() {}\n",
    )
    .unwrap();

    grapha()
        .args(["analyze", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("nodes"))
        .stdout(predicate::str::contains("edges"));
}

#[test]
fn index_and_entries_works() {
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

    grapha()
        .args(["flow", "entries", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("entries"));
}

#[test]
fn trace_command_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args(["flow", "trace", "main", "-p", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("entry"));
}

#[test]
fn reverse_command_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "trace",
            "helper",
            "--direction",
            "reverse",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("symbol"));
}

#[test]
fn reverse_trace_respects_depth_limit() {
    let dir = index_temp_project("fn main() { mid(); }\nfn mid() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "trace",
            "helper",
            "--direction",
            "reverse",
            "--depth",
            "1",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_entries\": 0"));

    grapha()
        .args([
            "flow",
            "trace",
            "helper",
            "--direction",
            "reverse",
            "--depth",
            "2",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_entries\": 1"));
}

#[test]
fn origin_command_reports_api_and_field_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("main.rs"),
        r#"
        fn fetch_profile() { helper(); }
        fn helper() {}
        fn display_name() { fetch_profile(); }
        fn title_text() { display_name(); }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        r#"
        [[classifiers]]
        regex = "fetch_profile"
        terminal = "network"
        direction = "read"
        operation = "fetch"
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
            "origin",
            "title_text",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_origins\""))
        .stdout(predicate::str::contains("\"origins\""))
        .stdout(predicate::str::contains("title_text"));
}

#[test]
fn origin_command_resolves_typealias_service_endpoint_without_registration() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("AppContext.swift"),
        r#"
        protocol UserAPI {
            func _getUser(id: Int, attrs: [String]) async throws -> String
        }

        protocol ServiceEventProtocol {}
        typealias ProfileAPI = UserAPI & ServiceEventProtocol
        typealias PublicProfileAPI = ProfileAPI

        struct AppContext {
            static let profile: any PublicProfileAPI = ProfileService()
        }

        extension UserAPI {
            func fetchUserInfo(id: Int) async throws -> String {
                try await _getUser(id: id, attrs: ["profile"])
            }
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ProfileModule.swift"),
        r#"
        struct ProfileService {}

        extension ProfileService: PublicProfileAPI {
            func _getUser(id: Int, attrs: [String]) async throws -> String {
                try await requestGetUser(id, attrs: attrs)
            }

            func requestGetUser(_ id: Int, attrs: [String]) async throws -> String {
                try await request("user/getUserInfoByUid/\(id)", data: ["attrs": attrs])
            }
        }

        func request(_ endpoint: String, data: [String: [String]]) async throws -> String {
            endpoint
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ProfilePageViewModel.swift"),
        r#"
        struct ProfilePageViewModel {
            var homeEffect: String = ""

            func refreshUserInfo() async throws {
                let _ = try await AppContext.profile.fetchUserInfo(id: 1)
            }

            func handleUserInfoUpdate(_ userInfo: String) {
                _ = userInfo
                _ = homeEffect
            }
        }
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
            "origin",
            "fetchUserInfo",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("requestGetUser"))
        .stdout(predicate::str::contains("user/getUserInfoByUid"))
        .stdout(predicate::str::contains("\"code_snippets\"").not())
        .stdout(predicate::str::contains("\"request_keys\""));

    grapha()
        .args([
            "flow",
            "origin",
            "fetchUserInfo",
            "-p",
            dir.path().to_str().unwrap(),
            "--fields",
            "snippet",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"code_snippets\""))
        .stdout(predicate::str::contains("\"reason\": \"request_leaf\""))
        .stdout(predicate::str::contains("\"request_keys\""));
}

#[test]
fn origin_command_accepts_network_terminal_filter() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("AppContext.swift"),
        r#"
        protocol UserAPI {
            func _getUser(id: Int, attrs: [String]) async throws -> String
        }

        protocol ServiceEventProtocol {}
        typealias ProfileAPI = UserAPI & ServiceEventProtocol
        typealias PublicProfileAPI = ProfileAPI

        struct AppContext {
            static let profile: any PublicProfileAPI = ProfileService()
        }

        extension UserAPI {
            func fetchUserInfo(id: Int) async throws -> String {
                try await _getUser(id: id, attrs: ["profile"])
            }
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("ProfileModule.swift"),
        r#"
        struct ProfileService {}

        extension ProfileService: PublicProfileAPI {
            func _getUser(id: Int, attrs: [String]) async throws -> String {
                try await requestGetUser(id, attrs: attrs)
            }

            func requestGetUser(_ id: Int, attrs: [String]) async throws -> String {
                try await request("user/getUserInfoByUid/\(id)", data: ["attrs": attrs])
            }
        }

        func request(_ endpoint: String, data: [String: [String]]) async throws -> String {
            endpoint
        }
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
            "origin",
            "fetchUserInfo",
            "-p",
            dir.path().to_str().unwrap(),
            "--terminal-kind",
            "network",
            "--fields",
            "snippet",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_origins\": 1"))
        .stdout(predicate::str::contains("\"terminal_kind\": \"network\""))
        .stdout(predicate::str::contains("requestGetUser"))
        .stdout(predicate::str::contains("\"code_snippets\""))
        .stdout(predicate::str::contains("user/getUserInfoByUid"));
}

#[test]
fn impact_command_defaults_to_json() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "symbol",
            "impact",
            "helper",
            "-p",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"source\""))
        .stdout(predicate::str::contains("\"total_affected\": 1"));
}

#[test]
fn context_tree_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "symbol",
            "context",
            "helper",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper [function] (main.rs)"))
        .stdout(predicate::str::contains("callers (1)"))
        .stdout(predicate::str::contains("main [function] (main.rs)"))
        .stdout(predicate::str::contains("└──"));
}

#[test]
fn entries_tree_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "entries",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("entry points (1 shown / 1 total)"))
        .stdout(predicate::str::contains("main [function] (main.rs)"))
        .stdout(predicate::str::contains("└──"));
}

#[test]
fn trace_tree_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "trace",
            "main",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("main [function] (main.rs)"))
        .stdout(predicate::str::contains("requested_symbol: main"))
        .stdout(predicate::str::contains("traced_roots: main"))
        .stdout(predicate::str::contains("fallback_used: false"))
        .stdout(predicate::str::contains(
            "summary: flows=0, reads=0, writes=0, async_crossings=0",
        ))
        .stdout(predicate::str::contains("flows (0)"));
}

#[test]
fn trace_brief_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "trace",
            "main",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "brief",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("trace: main [function] (main.rs)"))
        .stdout(predicate::str::contains(
            "summary: flows=0, reads=0, writes=0, async_crossings=0",
        ))
        .stdout(predicate::str::contains("requested_symbol: main"));
}

#[test]
fn flow_trace_swiftui_view_falls_back_to_body_or_actions() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");

    std::fs::write(
        dir.path().join("RoomPageCenterContentView.swift"),
        r#"
        import SwiftUI

        struct RoomPageCenterContentView: View {
            var body: some View {
                Button("Share") {
                    onShare()
                }
            }

            func onShare() {
                save()
            }

            func save() {
                UserDefaults.standard.set(true, forKey: "shared")
            }
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("grapha.toml"),
        r#"
        [[classifiers]]
        regex = "save"
        terminal = "persistence"
        direction = "write"
        operation = "save"
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
            "trace",
            "RoomPageCenterContentView",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "RoomPageCenterContentView [struct] (RoomPageCenterContentView.swift)",
        ))
        .stdout(predicate::str::contains(
            "requested_symbol: RoomPageCenterContentView",
        ))
        .stdout(predicate::str::contains("fallback_used: true"))
        .stdout(predicate::str::contains("traced_roots:"))
        .stdout(predicate::str::contains("onShare"))
        .stdout(predicate::str::contains("flows (3)"))
        .stdout(predicate::str::contains("onShare"))
        .stdout(predicate::str::contains(
            "save [terminal:persistence unknown unknown]",
        ));
}

#[test]
fn reverse_tree_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "flow",
            "trace",
            "helper",
            "--direction",
            "reverse",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper [function] (main.rs)"))
        .stdout(predicate::str::contains("affected entries (1)"))
        .stdout(predicate::str::contains(
            "main [entry] [function] (main.rs)",
        ))
        .stdout(predicate::str::contains("└──"))
        .stdout(predicate::str::contains("\"symbol\"").not());
}

#[test]
fn impact_tree_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "symbol",
            "impact",
            "helper",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("helper [function] (main.rs)"))
        .stdout(predicate::str::contains(
            "summary: depth_1=1, depth_2=0, depth_3_plus=0, total=1",
        ))
        .stdout(predicate::str::contains("dependents (1)"))
        .stdout(predicate::str::contains("main [function] (main.rs)"))
        .stdout(predicate::str::contains("└──"))
        .stdout(predicate::str::contains("\"source\"").not());
}

#[test]
fn impact_brief_format_works() {
    let dir = index_temp_project("fn main() { helper(); }\nfn helper() {}\n");

    grapha()
        .args([
            "symbol",
            "impact",
            "helper",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "brief",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "impact: helper [function] (main.rs)",
        ))
        .stdout(predicate::str::contains(
            "summary: total=1, depth_1=1, depth_2=0, depth_3_plus=0",
        ))
        .stdout(predicate::str::contains(
            "depth_1 (1): main [function] (main.rs)",
        ))
        .stdout(predicate::str::contains("\"source\"").not());
}

#[test]
fn context_tree_for_swiftui_body_shows_structure() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".grapha");
    std::fs::write(
        dir.path().join("ContentView.swift"),
        r#"
        import SwiftUI

        struct ContentView: View {
            var body: some View {
                VStack {
                    Text("Hello")
                }
            }
        }
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
            "symbol",
            "context",
            "body",
            "-p",
            dir.path().to_str().unwrap(),
            "--format",
            "tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "body [property] (ContentView.swift)",
        ))
        .stdout(predicate::str::contains("contains (1)"))
        .stdout(predicate::str::contains(
            "VStack [view] (ContentView.swift)",
        ))
        .stdout(predicate::str::contains("contained_by (1)"))
        .stdout(predicate::str::contains(
            "ContentView [struct] (ContentView.swift)",
        ));
}

#[test]
fn help_output_lists_new_command_tree() {
    grapha()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("symbol"))
        .stdout(predicate::str::contains("flow"))
        .stdout(predicate::str::contains("l10n"))
        .stdout(predicate::str::contains("repo"))
        .stdout(predicate::str::contains("reverse").not());

    grapha()
        .args(["symbol", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("context"))
        .stdout(predicate::str::contains("impact"));

    grapha()
        .args(["flow", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("trace"))
        .stdout(predicate::str::contains("graph"))
        .stdout(predicate::str::contains("entries"));

    grapha()
        .args(["l10n", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("symbol"))
        .stdout(predicate::str::contains("usages"));

    grapha()
        .args(["repo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("changes"));
}

#[test]
fn removed_top_level_commands_fail() {
    grapha()
        .args(["reverse"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));

    grapha()
        .args(["localize"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));

    grapha()
        .args(["dataflow"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}
