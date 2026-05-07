use std::ffi::OsString;
use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

pub fn global_data_root() -> PathBuf {
    global_data_root_from_env(
        |key| std::env::var_os(key),
        home_dir_from_env,
        current_platform,
    )
}

pub fn annotation_db_path(project_root: &Path) -> PathBuf {
    annotation_db_path_with_data_root(project_root, &global_data_root())
}

pub fn annotation_db_path_with_data_root(project_root: &Path, data_root: &Path) -> PathBuf {
    data_root
        .join("repos")
        .join(repo_id_for_project_root(project_root))
        .join("annotations.db")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub project_id: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
}

pub fn project_identity(project_root: &Path) -> ProjectIdentity {
    let project_id = repo_id_for_project_root(project_root);
    let Ok(repo) = Repository::discover(project_root) else {
        return ProjectIdentity {
            project_id,
            branch: "default".to_string(),
            head_oid: None,
            head_ref: None,
        };
    };

    let head = repo.head().ok();
    let head_oid = head
        .as_ref()
        .and_then(|head| head.target())
        .map(|oid| oid.to_string());
    let head_ref = head
        .as_ref()
        .and_then(|head| head.shorthand())
        .map(str::to_string);
    let branch = head_ref
        .clone()
        .or_else(|| {
            head_oid
                .as_deref()
                .map(|oid| format!("detached-{}", &oid[..12]))
        })
        .unwrap_or_else(|| "unborn".to_string());

    ProjectIdentity {
        project_id,
        branch,
        head_oid,
        head_ref,
    }
}

pub fn repo_id_for_project_root(project_root: &Path) -> String {
    let config = crate::config::load_config(project_root);
    if let Some(name) = config.repo.name.as_deref().and_then(non_empty_trimmed) {
        return repo_id_from_configured_name(name);
    }

    if let Ok(repo) = Repository::discover(project_root) {
        if let Some(remote_url) = primary_remote_url(&repo) {
            return repo_id_from_remote_url(&remote_url);
        }

        let common_dir = normalize_path_for_identity(repo.commondir());
        return format!("git-{}", hash_hex(common_dir.to_string_lossy().as_ref()));
    }

    let project_path = normalize_path_for_identity(project_root);
    format!("path-{}", hash_hex(project_path.to_string_lossy().as_ref()))
}

fn global_data_root_from_env<E, H, P>(mut env_var: E, home_dir: H, platform: P) -> PathBuf
where
    E: FnMut(&str) -> Option<OsString>,
    H: Fn() -> Option<PathBuf>,
    P: Fn() -> Platform,
{
    if let Some(home) = env_var("GRAPHA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home);
    }

    match platform() {
        Platform::MacOs => home_dir()
            .map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join("grapha")
            })
            .unwrap_or_else(|| PathBuf::from(".grapha").join("global")),
        Platform::Windows => env_var("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(home_dir)
            .map(|root| root.join("grapha"))
            .unwrap_or_else(|| PathBuf::from(".grapha").join("global")),
        Platform::Unix => env_var("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|root| root.join("grapha"))
            .or_else(|| home_dir().map(|home| home.join(".local").join("share").join("grapha")))
            .unwrap_or_else(|| PathBuf::from(".grapha").join("global")),
    }
}

fn home_dir_from_env() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }

    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Platform {
    MacOs,
    Windows,
    Unix,
}

fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::MacOs
    }

    #[cfg(windows)]
    {
        Platform::Windows
    }

    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        Platform::Unix
    }
}

fn primary_remote_url(repo: &Repository) -> Option<String> {
    repo.find_remote("origin")
        .ok()
        .and_then(|remote| remote.url().map(ToOwned::to_owned))
        .or_else(|| {
            let names = repo.remotes().ok()?;
            names
                .iter()
                .flatten()
                .find_map(|name| repo.find_remote(name).ok()?.url().map(ToOwned::to_owned))
        })
        .and_then(|url| non_empty_trimmed(&url).map(ToOwned::to_owned))
}

fn repo_id_from_configured_name(name: &str) -> String {
    let slug = slugify(name);
    format!("name-{slug}-{}", short_hash_hex(name))
}

fn repo_id_from_remote_url(url: &str) -> String {
    format!("remote-{}", hash_hex(&normalize_remote_url(url)))
}

fn normalize_remote_url(url: &str) -> String {
    let mut normalized = url.trim().replace('\\', "/");

    for prefix in ["https://", "http://", "ssh://"] {
        if let Some(stripped) = normalized.strip_prefix(prefix) {
            normalized = stripped.to_string();
            break;
        }
    }

    if let Some((user_host, path)) = normalized.split_once(':')
        && user_host.contains('@')
        && !path.starts_with("//")
    {
        let host = user_host.rsplit('@').next().unwrap_or(user_host);
        normalized = format!("{host}/{path}");
    } else if let Some((before_path, path)) = normalized.split_once('/') {
        let host = before_path.rsplit('@').next().unwrap_or(before_path);
        normalized = format!("{host}/{path}");
    }

    normalized = normalized.trim_matches('/').to_ascii_lowercase();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if let Some(stripped) = normalized.strip_suffix(".git") {
        normalized = stripped.to_string();
    }
    normalized
}

fn normalize_path_for_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in value.trim().chars() {
        let next = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            previous_dash = false;
            Some(ch)
        } else if ch == '-' || ch.is_whitespace() || ch == '/' || ch == '\\' {
            if previous_dash {
                None
            } else {
                previous_dash = true;
                Some('-')
            }
        } else if previous_dash {
            None
        } else {
            previous_dash = true;
            Some('-')
        };
        if let Some(ch) = next {
            slug.push(ch);
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "repo".to_string()
    } else {
        slug.to_string()
    }
}

fn short_hash_hex(value: &str) -> String {
    hash_hex(value)[..12].to_string()
}

fn hash_hex(value: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn grapha_home_overrides_default_data_root() {
        let root = PathBuf::from("/tmp/custom-grapha-home");
        let resolved = global_data_root_from_env(
            |key| (key == "GRAPHA_HOME").then(|| root.clone().into_os_string()),
            || Some(PathBuf::from("/tmp/home")),
            || Platform::MacOs,
        );

        assert_eq!(resolved, root);
    }

    #[test]
    fn configured_repo_name_drives_repo_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("grapha.toml"),
            "[repo]\nname = \"Frame UI\"\n",
        )
        .unwrap();

        let repo_id = repo_id_for_project_root(dir.path());

        assert!(repo_id.starts_with("name-Frame-UI-"));
    }

    #[test]
    fn remote_repo_identity_uses_normalized_url_hash() {
        assert_eq!(
            repo_id_from_remote_url("git@github.com:oops-rs/grapha.git"),
            repo_id_from_remote_url("https://github.com/oops-rs/grapha/")
        );
    }

    #[test]
    fn git_common_dir_identity_is_shared_by_worktrees_without_remote() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        std::fs::write(main.join("lib.rs"), "pub struct Shared;\n").unwrap();

        run_git(&main, &["init"]);
        run_git(&main, &["add", "lib.rs"]);
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

        assert_eq!(
            repo_id_for_project_root(&main),
            repo_id_for_project_root(&linked)
        );
    }

    #[test]
    fn fallback_repo_identity_uses_project_path_hash() {
        let dir = tempfile::tempdir().unwrap();
        let repo_id = repo_id_for_project_root(dir.path());

        assert!(repo_id.starts_with("path-"));
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
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
}
