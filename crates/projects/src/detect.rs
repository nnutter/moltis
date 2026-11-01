use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use tracing::debug;

use crate::types::Project;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Derive a human-friendly project label from a directory.
///
/// Resolution order:
/// 1. Timber worktree identity (`<branch>@<repo>`)
/// 2. `Cargo.toml` package name
/// 3. `package.json` name
/// 4. Git remote origin name (last path segment)
/// 5. Directory name
pub fn derive_label(dir: &Path) -> String {
    // Timber checkouts of one repo share manifests and remotes, so the
    // worktree identity must win to keep labels unique.
    if let Some((branch, repo)) = timber_identity(dir) {
        return format!("{branch}@{repo}");
    }
    // Try Cargo.toml
    if let Some(name) = cargo_name(dir) {
        return name;
    }
    // Try package.json
    if let Some(name) = package_json_name(dir) {
        return name;
    }
    // Try git remote
    if let Some(name) = git_remote_name(dir) {
        return name;
    }
    // Fallback to directory name
    dir.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn cargo_name(dir: &Path) -> Option<String> {
    let path = dir.join("Cargo.toml");
    let content = fs::read_to_string(path).ok()?;
    let table: toml::Table = toml::from_str(&content).ok()?;
    table
        .get("package")?
        .get("name")?
        .as_str()
        .map(String::from)
}

fn package_json_name(dir: &Path) -> Option<String> {
    let path = dir.join("package.json");
    let content = fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("name")?.as_str().map(String::from)
}

/// Resolve the git dir for `dir`, handling both plain repositories (a `.git`
/// directory) and linked worktrees (a `.git` file containing `gitdir: <path>`).
fn resolve_git_dir(dir: &Path) -> Option<PathBuf> {
    let dotgit = dir.join(".git");
    if dotgit.is_dir() {
        return Some(dotgit);
    }
    let content = fs::read_to_string(&dotgit).ok()?;
    let gitdir = content.strip_prefix("gitdir: ")?.trim();
    if gitdir.is_empty() {
        return None;
    }
    let gitdir = PathBuf::from(gitdir);
    Some(if gitdir.is_relative() {
        dir.join(gitdir)
    } else {
        gitdir
    })
}

fn git_remote_name(dir: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(dir)?;
    // Linked worktrees keep shared config in the common dir.
    let commondir_file = git_dir.join("commondir");
    let config_path = if let Ok(common) = fs::read_to_string(&commondir_file) {
        let common = common.trim();
        if common.is_empty() {
            git_dir.join("config")
        } else {
            let common = PathBuf::from(common);
            let common = if common.is_relative() {
                git_dir.join(common)
            } else {
                common
            };
            common.join("config")
        }
    } else {
        git_dir.join("config")
    };
    let content = fs::read_to_string(config_path).ok()?;
    // Simple parse: find url = ... line under [remote "origin"]
    let mut in_origin = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[remote") && trimmed.contains("\"origin\"") {
            in_origin = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_origin = false;
            continue;
        }
        if in_origin
            && trimmed.starts_with("url")
            && let Some(url) = trimmed.split('=').nth(1)
        {
            let url = url.trim();
            // Extract repo name from URL like git@...:user/repo.git or https://.../repo.git
            let name = url
                .rsplit('/')
                .next()
                .or_else(|| url.rsplit(':').next())
                .unwrap_or(url)
                .trim_end_matches(".git");
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// If `dir` is a timber-managed worktree, return `(branch, repo)` — e.g.
/// `("fix-signal", "moltis")` for `~/worktrees/moltis/fix-signal/moltis`.
///
/// Derived from the `.git` gitdir path (which embeds the timber repo name)
/// and its `HEAD` (which tracks the worktree branch), so no `timber`
/// binary is needed. Returns `None` for plain repos and detached checkouts.
fn timber_identity(dir: &Path) -> Option<(String, String)> {
    let git_dir = resolve_git_dir(dir)?;
    let git_dir_str = git_dir.to_string_lossy();
    // Timber gitdirs look like `.../timber/repos/<repo>.git/worktrees/<name>`
    // where `<repo>` may itself contain `/` (e.g. `nnutter/moltisctl`).
    let after_repos = git_dir_str.split("/repos/").nth(1)?;
    let repo = after_repos.split(".git/worktrees/").next()?;
    if repo.is_empty() {
        return None;
    }
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    if branch.is_empty() {
        return None;
    }
    Some((branch.to_string(), repo.to_string()))
}

/// Derive a slug-friendly ID from a directory path.
///
/// Timber worktrees share their checkout directory name with sibling
/// worktrees of the same repo, so their ID is slugged from the timber
/// identity (`<branch>@<repo>`) instead.
pub fn derive_id(dir: &Path) -> String {
    if let Some((branch, repo)) = timber_identity(dir) {
        return slugify(&format!("{branch}@{repo}"));
    }
    slugify(&dir.file_name().unwrap_or_default().to_string_lossy())
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
}

/// Check if a directory is a git repository.
pub fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Detect a project from a directory. Returns `None` if directory doesn't exist.
pub fn detect_project(dir: &Path) -> Option<Project> {
    if !dir.is_dir() {
        return None;
    }
    let now = now_ms();
    let label = derive_label(dir);
    let id = derive_id(dir);
    debug!(dir = %dir.display(), label = %label, "detected project");
    Some(Project {
        id,
        label,
        directory: dir.to_path_buf(),
        system_prompt: None,
        auto_worktree: is_git_repo(dir),
        setup_command: None,
        teardown_command: None,
        branch_prefix: None,
        sandbox_image: None,
        detected: true,
        code_index_enabled: true,
        created_at: now,
        updated_at: now,
    })
}

/// Well-known parent directories where projects typically live.
const SCAN_PARENTS: &[&str] = &[
    "Projects",
    "Developer",
    "src",
    "code",
    "repos",
    "workspace",
    "dev",
    "git",
];

/// Return the immediate children of well-known project parent directories
/// under the user's home folder. Only existing directories are included.
pub fn default_scan_dirs() -> Vec<PathBuf> {
    let Ok(home) = std::env::var("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    scan_dirs_from_home(&home)
}

/// Scan well-known project parent directories under the given home path.
///
/// Extracted from [`default_scan_dirs`] so tests can pass a custom home
/// directory without mutating the process environment.
fn scan_dirs_from_home(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for parent_name in SCAN_PARENTS {
        let parent = home.join(parent_name);
        if let Ok(entries) = fs::read_dir(&parent) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.push(path);
                }
            }
        }
    }
    // Superset worktrees: ~/.superset/worktrees/<project>/<branch>/
    let superset_wt = home.join(".superset").join("worktrees");
    if let Ok(projects) = fs::read_dir(&superset_wt) {
        for project in projects.flatten() {
            if project.path().is_dir()
                && let Ok(branches) = fs::read_dir(project.path())
            {
                for branch in branches.flatten() {
                    let path = branch.path();
                    if path.is_dir() {
                        out.push(path);
                    }
                }
            }
        }
    }
    // Timber worktrees: ~/worktrees/<repo>/<worktree>/<checkout>. Both repo
    // and worktree names may contain `/`, so scan recursively for git repos.
    let timber_wt = home.join("worktrees");
    if timber_wt.is_dir() {
        collect_git_repos(&timber_wt, &mut out);
    }
    out
}

/// Directory names never descended into while scanning for git repositories.
/// Build outputs and session worktrees cannot be project roots.
const SCAN_SKIP_DIRS: &[&str] = &[
    ".git",
    ".moltis-worktrees",
    "node_modules",
    "target",
    "DerivedData",
    ".build",
    "dist",
];

/// Depth of traversal below the scan root. Timber nests checkouts as
/// `<repo>/<worktree>/<checkout>` where repo and worktree names may each
/// contain `/`, so plain two-level expansion is not enough.
const SCAN_MAX_DEPTH: usize = 8;

/// Collect every git repository under `root`, including ones nested inside
/// other checkouts. Symlinks are not followed.
fn collect_git_repos(root: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SCAN_SKIP_DIRS.contains(&n))
            {
                continue;
            }
            if is_git_repo(&path) {
                out.push(path.clone());
            }
            if depth + 1 < SCAN_MAX_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
}
/// Scan a list of directories and detect projects from git repos.
/// Returns new projects not already in `known_ids`.
pub fn auto_detect(dirs: &[&Path], known_ids: &[String]) -> Vec<Project> {
    let mut detected = Vec::new();
    for dir in dirs {
        if !is_git_repo(dir) {
            continue;
        }
        if let Some(project) = detect_project(dir)
            && !known_ids.contains(&project.id)
        {
            detected.push(project);
        }
    }
    detected
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_label_cargo() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(derive_label(dir.path()), "my-crate");
    }

    #[test]
    fn test_derive_label_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name": "my-app"}"#).unwrap();
        assert_eq!(derive_label(dir.path()), "my-app");
    }

    #[test]
    fn test_derive_label_fallback_to_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let label = derive_label(dir.path());
        // tempdir names are random but non-empty
        assert!(!label.is_empty());
    }

    #[test]
    fn test_is_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(dir.path()));
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(is_git_repo(dir.path()));
    }

    #[test]
    fn test_detect_project() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"cool-proj\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let p = detect_project(dir.path()).unwrap();
        assert_eq!(p.label, "cool-proj");
        assert!(p.auto_worktree);
        assert!(p.detected);
    }

    #[test]
    fn test_git_remote_name_linked_worktree() {
        // Simulate `git worktree add`: `.git` is a file pointing at a
        // per-worktree gitdir whose `commondir` holds the shared config.
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().join("common.git");
        fs::create_dir(&common).unwrap();
        fs::write(
            common.join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/my-repo.git\n",
        )
        .unwrap();
        let wt_gitdir = tmp.path().join("wt-gitdir");
        fs::create_dir(&wt_gitdir).unwrap();
        fs::write(wt_gitdir.join("commondir"), common.to_str().unwrap()).unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}", wt_gitdir.display())).unwrap();

        assert_eq!(git_remote_name(&wt).as_deref(), Some("my-repo"));
    }

    #[test]
    fn test_timber_label_and_id() {
        // Fake timber gitdir: <tmp>/repos/moltis.git/worktrees/moltis/HEAD.
        let tmp = tempfile::tempdir().unwrap();
        let gitdir = tmp
            .path()
            .join("repos")
            .join("moltis.git")
            .join("worktrees")
            .join("moltis");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/fix-signal\n").unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}", gitdir.display())).unwrap();
        // A Cargo.toml name must not shadow the worktree identity.
        fs::write(wt.join("Cargo.toml"), "[package]\nname = \"moltis\"\n").unwrap();

        assert_eq!(derive_label(&wt), "fix-signal@moltis");
        assert_eq!(derive_id(&wt), "fix-signal-moltis");
        let p = detect_project(&wt).unwrap();
        assert_eq!(p.label, "fix-signal@moltis");
        assert_eq!(p.id, "fix-signal-moltis");
    }

    #[test]
    fn test_timber_slash_names() {
        let tmp = tempfile::tempdir().unwrap();
        let gitdir = tmp
            .path()
            .join("repos")
            .join("NousResearch")
            .join("hermes-agent.git")
            .join("worktrees")
            .join("hermes-agent");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            gitdir.join("HEAD"),
            "ref: refs/heads/BearHuddleston/feat/desktop-web\n",
        )
        .unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}", gitdir.display())).unwrap();

        assert_eq!(
            derive_label(&wt),
            "BearHuddleston/feat/desktop-web@NousResearch/hermes-agent"
        );
        assert_eq!(
            derive_id(&wt),
            "bearhuddleston-feat-desktop-web-nousresearch-hermes-agent"
        );
    }

    #[test]
    fn test_timber_detached_head_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let gitdir = tmp
            .path()
            .join("repos")
            .join("moltis.git")
            .join("worktrees")
            .join("moltis");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(
            gitdir.join("HEAD"),
            "2c1541f149d9ca2e48271efd22369577326efa97\n",
        )
        .unwrap();
        let wt = tmp.path().join("my-checkout");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}", gitdir.display())).unwrap();

        assert!(timber_identity(&wt).is_none());
        assert_eq!(derive_label(&wt), "my-checkout");
    }

    #[test]
    fn test_scan_dirs_finds_timber_worktrees() {
        let home = tempfile::tempdir().unwrap();
        let mk_repo = |rel: &str| {
            let dir = home.path().join(rel);
            fs::create_dir_all(&dir).unwrap();
            fs::create_dir(dir.join(".git")).unwrap();
            dir
        };
        // Timber-shaped nesting: <repo>/<worktree>/<checkout>.
        let wt_a = mk_repo("worktrees/moltis/fix-signal/moltis");
        let wt_b = mk_repo(
            "worktrees/NousResearch/hermes-agent/BearHuddleston/feat/desktop-web/hermes-agent",
        );
        // Nested checkout inside another checkout is still found.
        let nested = mk_repo("worktrees/nnutter/moltisctl/moltisctl/moltisctl/shelleyctl");
        // Build outputs and session worktrees are skipped.
        mk_repo("worktrees/moltis/fix-signal/moltis/target/should-skip");
        mk_repo("worktrees/moltis/fix-signal/moltis/.moltis-worktrees/session1");

        let dirs = scan_dirs_from_home(home.path());
        assert!(dirs.contains(&wt_a));
        assert!(dirs.contains(&wt_b));
        assert!(dirs.contains(&nested));
        assert!(!dirs.iter().any(|p| p.ends_with("should-skip")));
        assert!(!dirs.iter().any(|p| p.ends_with("session1")));
    }

    #[test]
    fn test_derive_id() {
        let id = derive_id(Path::new("/home/user/My Project!"));
        assert_eq!(id, "my-project-");
    }

    #[test]
    fn test_default_scan_dirs_with_projects_folder() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("Projects");
        fs::create_dir(&projects).unwrap();
        let repo_a = projects.join("repo-a");
        fs::create_dir(&repo_a).unwrap();
        let repo_b = projects.join("repo-b");
        fs::create_dir(&repo_b).unwrap();
        // Also create a file — should be ignored.
        fs::write(projects.join("not-a-dir.txt"), "hi").unwrap();

        let dirs = scan_dirs_from_home(home.path());
        assert!(dirs.contains(&repo_a));
        assert!(dirs.contains(&repo_b));
        assert!(!dirs.iter().any(|p| p.ends_with("not-a-dir.txt")));
    }

    #[test]
    fn test_default_scan_dirs_no_matching_parents() {
        let home = tempfile::tempdir().unwrap();
        let dirs = scan_dirs_from_home(home.path());
        assert!(dirs.is_empty());
    }
}
