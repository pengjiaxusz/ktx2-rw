use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::constant::KTX_SOFTWARE_VERSION;

const PACKAGE_NAME: &str = "ktx2-rw";
const PACKAGE_PREFIX: &str = "ktx2-rw-";
const OUTPUT_DIR_NAME: &str = "out";
const KTX_BUILD_DIR_NAME: &str = "KTX-Software-build";
const KTX_BINDINGS_FILE_NAME: &str = "bindings.rs";
const LLVM_COV_TARGET_DIR: &str = "llvm-cov-target";
const MIRI_TARGET_DIR: &str = "miri";
const BUILD_DIR: &str = "build";
const DEBUG_DIR: &str = "debug";
const RELEASE_DIR: &str = "release";

pub fn try_reuse_cached_build(current_out_dir: &Path) -> bool {
    if let Some(cache) = find_cached_build(current_out_dir)
        && cache.validate()
        && cache.restore_to(current_out_dir)
    {
        println!(
            "cargo:warning=ktx2-rw: Successfully reused cached build artifacts from {}",
            cache.ktx_build_dir.display()
        );
        return true;
    }
    false
}

struct CachedBuild {
    ktx_build_dir: PathBuf,
    ktx_source_dir: PathBuf,
    bindings_file: PathBuf,
}

impl CachedBuild {
    fn from_package_dir(package_dir: &Path) -> Self {
        let output_dir = package_dir.join(OUTPUT_DIR_NAME);
        Self {
            ktx_build_dir: output_dir.join(KTX_BUILD_DIR_NAME),
            ktx_source_dir: output_dir.join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION)),
            bindings_file: output_dir.join(KTX_BINDINGS_FILE_NAME),
        }
    }

    fn validate(&self) -> bool {
        // 1. Build directory exists
        if !self.ktx_build_dir.is_dir() {
            return false;
        }

        // 2. Library file exists in lib/ and is non-empty (>0 bytes)
        let lib_dir = self.ktx_build_dir.join("lib");
        if !lib_dir.is_dir() {
            return false;
        }
        let valid_lib_exists = ["ktx.lib", "libktx.a", "libktx.dylib", "libktx.so"]
            .iter()
            .any(|name| {
                let file = lib_dir.join(name);
                file.is_file() && fs::metadata(&file).is_ok_and(|m| m.len() > 0)
            })
            || lib_dir.join("ktx.framework").exists();
        if !valid_lib_exists {
            return false;
        }

        // 3. Source headers exist (for bindgen and compilation)
        if !self.ktx_source_dir.is_dir() {
            return false;
        }
        if !self.ktx_source_dir.join("include").join("ktx.h").is_file() {
            return false;
        }

        // 4. Bindings file exists and is non-empty
        if !self.bindings_file.is_file() {
            return false;
        }
        if !fs::metadata(&self.bindings_file).is_ok_and(|m| m.len() > 0) {
            return false;
        }

        true
    }

    fn restore_to(&self, target: &Path) -> bool {
        let dst_build = target.join(KTX_BUILD_DIR_NAME);
        let dst_source = target.join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION));
        let dst_bindings = target.join(KTX_BINDINGS_FILE_NAME);

        // Clean up any stale partial targets before copying
        let _ = fs::remove_dir_all(&dst_build);
        let _ = fs::remove_dir_all(&dst_source);
        let _ = fs::remove_file(&dst_bindings);

        let ok_build = copy_directory(&self.ktx_build_dir, &dst_build);
        let ok_source = copy_directory(&self.ktx_source_dir, &dst_source);
        let ok_bindings = fs::copy(&self.bindings_file, &dst_bindings).is_ok();

        if ok_build && ok_source && ok_bindings {
            true
        } else {
            // Roll back on failure to avoid leaving a corrupted partial cache
            let _ = fs::remove_dir_all(&dst_build);
            let _ = fs::remove_dir_all(&dst_source);
            let _ = fs::remove_file(&dst_bindings);
            false
        }
    }
}

struct BuildContext {
    current_build_root: PathBuf,
    current_target_root: PathBuf,
}

impl BuildContext {
    fn from_out_dir(out_dir: &Path) -> Option<Self> {
        // Walk up from out_dir to locate the ancestor directory named "build".
        // This handles both legacy Cargo layout (target/debug/build/ktx2-rw-<hash>/out)
        // and modern Cargo layout (target/debug/build/ktx2-rw/<hash>/out).
        let current_build_root = find_ancestor_named(out_dir.parent()?, BUILD_DIR)?;
        let profile_dir = current_build_root.parent()?;
        let current_target_root = profile_dir.parent()?.to_path_buf();

        Some(Self {
            current_build_root,
            current_target_root,
        })
    }

    fn is_llvm_cov_build(&self) -> bool {
        self.current_target_root
            .file_name()
            .is_some_and(|name| name == LLVM_COV_TARGET_DIR)
    }

    fn is_miri_build(&self) -> bool {
        let target_name = self.current_target_root.file_name();
        if target_name.is_some_and(|name| name == MIRI_TARGET_DIR) {
            return true;
        }
        self.is_miri_build_with_target()
    }

    fn is_miri_build_with_target(&self) -> bool {
        self.current_target_root
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name == MIRI_TARGET_DIR)
    }

    fn is_special_build(&self) -> bool {
        self.is_llvm_cov_build() || self.is_miri_build()
    }

    fn is_release(&self) -> bool {
        self.current_build_root
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name == RELEASE_DIR)
    }

    fn cargo_build_root(&self) -> PathBuf {
        if !self.is_special_build() {
            return self.current_build_root.clone();
        }

        let cargo_target_root = if self.is_miri_build_with_target() {
            self.current_target_root
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or(&self.current_target_root)
        } else {
            self.current_target_root
                .parent()
                .unwrap_or(&self.current_target_root)
        };

        let profile = if self.is_release() {
            RELEASE_DIR
        } else {
            DEBUG_DIR
        };
        cargo_target_root.join(profile).join(BUILD_DIR)
    }
}

fn find_ancestor_named(start: &Path, name: &str) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.file_name().is_some_and(|n| n == name) {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn find_cached_build(current_out_dir: &Path) -> Option<CachedBuild> {
    let context = BuildContext::from_out_dir(current_out_dir)?;
    let build_root = context.cargo_build_root();

    // 1. Search the current profile's build directory first
    if let Some(found) = search_cache_in_directory(&build_root, current_out_dir) {
        return Some(found);
    }

    // 2. Search sibling profile's build directory in the same target
    if let Some(sibling_root) = sibling_build_root(&build_root, context.is_release())
        && sibling_root != build_root
        && let Some(found) = search_cache_in_directory(&sibling_root, current_out_dir)
    {
        return Some(found);
    }

    // 3. Search across sibling Git worktrees of the same repository
    if let Some(found) = search_cache_in_worktrees(current_out_dir, &context) {
        return Some(found);
    }

    None
}

fn sibling_build_root(build_root: &Path, is_release: bool) -> Option<PathBuf> {
    let parent = build_root.parent()?; // e.g. target/debug
    let grandparent = parent.parent()?; // e.g. target or target/llvm-cov-target
    let sibling_profile = if is_release { DEBUG_DIR } else { RELEASE_DIR };
    Some(grandparent.join(sibling_profile).join(BUILD_DIR))
}

fn search_cache_in_directory(build_root: &Path, current_out_dir: &Path) -> Option<CachedBuild> {
    if !build_root.is_dir() {
        return None;
    }

    // 1. Check modern Cargo layout: build_root/ktx2-rw/<hash>/out
    let modern_pkg_dir = build_root.join(PACKAGE_NAME);
    if modern_pkg_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&modern_pkg_dir)
    {
        for entry in entries.filter_map(Result::ok) {
            let candidate = entry.path();
            if candidate.is_dir() && is_candidate_package(&candidate, current_out_dir) {
                let cache = CachedBuild::from_package_dir(&candidate);
                if cache.validate() {
                    return Some(cache);
                }
            }
        }
    }

    // 2. Check legacy Cargo layout: build_root/ktx2-rw-<hash>/out
    if let Ok(entries) = fs::read_dir(build_root) {
        for entry in entries.filter_map(Result::ok) {
            let candidate = entry.path();
            let is_legacy = candidate
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PACKAGE_PREFIX));
            if is_legacy && candidate.is_dir() && is_candidate_package(&candidate, current_out_dir)
            {
                let cache = CachedBuild::from_package_dir(&candidate);
                if cache.validate() {
                    return Some(cache);
                }
            }
        }
    }

    None
}

fn is_candidate_package(candidate_path: &Path, current_out_dir: &Path) -> bool {
    let current_package_dir = match current_out_dir.parent() {
        Some(p) => p,
        None => return false,
    };
    if candidate_path == current_package_dir {
        return false;
    }
    if let (Ok(c1), Ok(c2)) = (
        fs::canonicalize(candidate_path),
        fs::canonicalize(current_package_dir),
    ) && c1 == c2
    {
        return false;
    }
    true
}

/// Locate the root of the Git repository that contains `start_dir`.
fn find_git_root(start_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(start_dir);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Discover all sibling Git worktrees (excluding `current_git_root`).
fn find_sibling_worktrees(current_git_root: &Path) -> Vec<PathBuf> {
    let mut worktrees = Vec::new();

    // Primary method: query git CLI
    if let Ok(output) = Command::new("git")
        .args([
            "-C",
            current_git_root.to_str().unwrap_or("."),
            "worktree",
            "list",
            "--porcelain",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                let path = PathBuf::from(path_str.trim());
                if path.is_dir() {
                    worktrees.push(path);
                }
            }
        }
    }

    // Fallback: parse .git file/directory structure directly from filesystem
    if worktrees.is_empty() {
        worktrees = find_worktrees_from_filesystem(current_git_root);
    }

    // Filter out the current git root using canonicalized path comparison
    let canonical_root = fs::canonicalize(current_git_root).ok();
    worktrees.retain(|wt| {
        if let (Some(c_root), Ok(c_wt)) = (&canonical_root, fs::canonicalize(wt)) {
            c_root != &c_wt
        } else {
            wt != current_git_root
        }
    });

    worktrees
}

/// Filesystem fallback for discovering worktrees when `git` command is unavailable.
fn find_worktrees_from_filesystem(git_root: &Path) -> Vec<PathBuf> {
    let mut worktrees = Vec::new();
    let git_entry = git_root.join(".git");

    let common_git_dir = if git_entry.is_file() {
        // Linked worktree: .git file contains "gitdir: <path>"
        let Ok(content) = fs::read_to_string(&git_entry) else {
            return worktrees;
        };
        let Some(gitdir_line) = content.lines().find(|l| l.starts_with("gitdir:")) else {
            return worktrees;
        };
        let Some(gitdir_path_str) = gitdir_line.strip_prefix("gitdir:") else {
            return worktrees;
        };
        let gitdir_path = PathBuf::from(gitdir_path_str.trim());

        // Read commondir to find main repository .git
        let commondir_file = gitdir_path.join("commondir");
        if let Ok(commondir_rel) = fs::read_to_string(&commondir_file) {
            gitdir_path.join(commondir_rel.trim())
        } else {
            gitdir_path
                .parent()
                .and_then(|p| p.parent())
                .map(PathBuf::from)
                .unwrap_or(gitdir_path)
        }
    } else if git_entry.is_dir() {
        // Main worktree: .git is a directory
        git_entry
    } else {
        return worktrees;
    };

    // Main worktree root is the parent of common_git_dir
    if let Some(main_wt) = common_git_dir.parent()
        && main_wt.is_dir()
    {
        worktrees.push(main_wt.to_path_buf());
    }

    // Linked worktrees are listed in common_git_dir/worktrees/
    let worktrees_dir = common_git_dir.join("worktrees");
    if let Ok(entries) = fs::read_dir(worktrees_dir) {
        for entry in entries.filter_map(Result::ok) {
            let gitdir_file = entry.path().join("gitdir");
            if let Ok(gitdir_content) = fs::read_to_string(gitdir_file) {
                let target_git = PathBuf::from(gitdir_content.trim());
                if let Some(wt_root) = target_git.parent()
                    && wt_root.is_dir()
                {
                    worktrees.push(wt_root.to_path_buf());
                }
            }
        }
    }

    worktrees
}

/// Find all candidate target directories in a worktree (standard "target" and custom "target-*").
fn find_target_directories(worktree_dir: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let default_target = worktree_dir.join("target");
    if default_target.is_dir() {
        targets.push(default_target);
    }

    // Scan for custom target directories such as target-* (e.g. target-task per parallel worktree conventions)
    if let Ok(entries) = fs::read_dir(worktree_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with("target")
                && path != worktree_dir.join("target")
            {
                targets.push(path);
            }
        }
    }

    targets
}

/// Search for valid cached builds across sibling Git worktrees.
fn search_cache_in_worktrees(
    current_out_dir: &Path,
    context: &BuildContext,
) -> Option<CachedBuild> {
    let git_root = find_git_root(&context.current_target_root)?;
    let sibling_worktrees = find_sibling_worktrees(&git_root);

    let profile = if context.is_release() {
        RELEASE_DIR
    } else {
        DEBUG_DIR
    };
    let sibling_profile = if context.is_release() {
        DEBUG_DIR
    } else {
        RELEASE_DIR
    };

    for wt in sibling_worktrees {
        let targets = find_target_directories(&wt);
        for target_dir in targets {
            // 1. Check matching profile first
            let primary_build_root = target_dir.join(profile).join(BUILD_DIR);
            if let Some(cache) = search_cache_in_directory(&primary_build_root, current_out_dir) {
                return Some(cache);
            }

            // 2. Check sibling profile (C library is identical for debug and release)
            let sibling_build_root = target_dir.join(sibling_profile).join(BUILD_DIR);
            if let Some(cache) = search_cache_in_directory(&sibling_build_root, current_out_dir) {
                return Some(cache);
            }
        }
    }

    None
}

fn copy_directory(source: &Path, destination: &Path) -> bool {
    if fs::create_dir_all(destination).is_err() {
        return false;
    }
    let Ok(entries) = fs::read_dir(source) else {
        return false;
    };
    entries.filter_map(Result::ok).all(|entry| {
        let src = entry.path();
        let dst = destination.join(entry.file_name());
        entry.file_type().is_ok_and(|ty| {
            if ty.is_dir() {
                copy_directory(&src, &dst)
            } else {
                fs::copy(&src, &dst).is_ok()
            }
        })
    })
}
