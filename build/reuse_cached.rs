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
const LIB_DIR_NAME: &str = "lib";
const INCLUDE_DIR_NAME: &str = "include";

const ENV_NO_CACHE_REUSE: &str = "KTX_NO_CACHE_REUSE";
const ENV_NO_WORKTREE_CACHE: &str = "KTX_NO_WORKTREE_CACHE";

fn is_env_flag_set(var_name: &str) -> bool {
    std::env::var(var_name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub fn try_reuse_cached_build(current_out_dir: &Path) -> bool {
    if is_env_flag_set(ENV_NO_CACHE_REUSE) {
        return false;
    }

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

        // 2. Library directory exists
        let lib_dir = self.ktx_build_dir.join(LIB_DIR_NAME);
        if !lib_dir.is_dir() {
            return false;
        }

        // 3. Expected library for current TARGET_OS exists, is readable, and non-empty
        if !has_valid_target_library(&lib_dir) {
            return false;
        }

        // 4. Source headers exist (for bindgen and compilation)
        if !self.ktx_source_dir.is_dir() {
            return false;
        }
        if !self
            .ktx_source_dir
            .join(INCLUDE_DIR_NAME)
            .join("ktx.h")
            .is_file()
        {
            return false;
        }

        // 5. Bindings file exists, is non-empty (>0 bytes), and readable
        if !is_valid_non_empty_file(&self.bindings_file) {
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

        // Copy only the essential library and header artifacts.
        // We purposefully skip KTX-Software-build/build (which contains 30MB+ of intermediate
        // CMake files, .obj, .vcxproj, and symlinks that can break cross-compilation on macOS).
        let src_lib = self.ktx_build_dir.join(LIB_DIR_NAME);
        let dst_lib = dst_build.join(LIB_DIR_NAME);
        let ok_lib = copy_directory(&src_lib, &dst_lib);

        // Optionally copy include directory if present in the build tree
        let src_include = self.ktx_build_dir.join(INCLUDE_DIR_NAME);
        if src_include.is_dir() {
            let _ = copy_directory(&src_include, &dst_build.join(INCLUDE_DIR_NAME));
        }

        let ok_source = copy_directory(&self.ktx_source_dir, &dst_source);
        let ok_bindings = fs::copy(&self.bindings_file, &dst_bindings).is_ok();

        if ok_lib && ok_source && ok_bindings {
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

fn has_valid_target_library(lib_dir: &Path) -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    match target_os.as_str() {
        "windows" => {
            is_valid_non_empty_file(&lib_dir.join("ktx.lib"))
                || is_valid_non_empty_file(&lib_dir.join("libktx.a"))
        }
        "macos" | "ios" => {
            is_valid_non_empty_file(&lib_dir.join("libktx.a"))
                || is_valid_non_empty_file(&lib_dir.join("libktx.dylib"))
                || lib_dir.join("ktx.framework").exists()
        }
        _ => {
            // Linux, Android, Musl, FreeBSD, etc.
            is_valid_non_empty_file(&lib_dir.join("libktx.a"))
                || is_valid_non_empty_file(&lib_dir.join("libktx.so"))
        }
    }
}

fn is_valid_non_empty_file(path: &Path) -> bool {
    path.is_file() && fs::metadata(path).is_ok_and(|m| m.len() > 0) && fs::File::open(path).is_ok()
}

struct BuildContext {
    current_build_root: PathBuf,
    current_target_root: PathBuf,
    target_triple: Option<String>,
}

impl BuildContext {
    fn from_out_dir(out_dir: &Path) -> Option<Self> {
        // Walk up from out_dir to locate the ancestor directory named "build".
        // This handles both legacy Cargo layout (target/debug/build/ktx2-rw-<hash>/out)
        // and modern Cargo layout (target/debug/build/ktx2-rw/<hash>/out).
        let current_build_root = find_ancestor_named(out_dir.parent()?, BUILD_DIR)?;
        let profile_dir = current_build_root.parent()?;
        let parent_dir = profile_dir.parent()?;

        let current_target = std::env::var("TARGET").ok();
        let (current_target_root, target_triple) = if let Some(ref target) = current_target
            && parent_dir.file_name().and_then(|n| n.to_str()) == Some(target)
        {
            (parent_dir.parent()?.to_path_buf(), Some(target.clone()))
        } else {
            (parent_dir.to_path_buf(), None)
        };

        Some(Self {
            current_build_root,
            current_target_root,
            target_triple,
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

        if let Some(ref triple) = self.target_triple {
            cargo_target_root.join(triple).join(profile).join(BUILD_DIR)
        } else {
            cargo_target_root.join(profile).join(BUILD_DIR)
        }
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

    // 3. Search across sibling Git worktrees (unless opted out)
    if !is_env_flag_set(ENV_NO_WORKTREE_CACHE)
        && let Some(found) = search_cache_in_worktrees(current_out_dir, &context)
    {
        return Some(found);
    }

    None
}

fn sibling_build_root(build_root: &Path, is_release: bool) -> Option<PathBuf> {
    let parent = build_root.parent()?; // e.g. target/debug or target/<triple>/debug
    let grandparent = parent.parent()?; // e.g. target or target/<triple>
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
    !is_same_path(candidate_path, current_package_dir)
}

fn is_same_path(p1: &Path, p2: &Path) -> bool {
    if p1 == p2 {
        return true;
    }
    if let (Ok(c1), Ok(c2)) = (fs::canonicalize(p1), fs::canonicalize(p2)) {
        if c1 == c2 {
            return true;
        }
        #[cfg(windows)]
        if c1
            .to_string_lossy()
            .eq_ignore_ascii_case(&c2.to_string_lossy())
        {
            return true;
        }
    }
    #[cfg(windows)]
    {
        let s1 = p1.to_string_lossy().replace('/', "\\");
        let s2 = p2.to_string_lossy().replace('/', "\\");
        if s1.eq_ignore_ascii_case(&s2) {
            return true;
        }
    }
    false
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

    // Filter out current git root using normalized path comparison
    worktrees.retain(|wt| !is_same_path(wt, current_git_root));

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
            // If the current build specifies a target triple (cross-compilation),
            // strictly search within the matching triple directory to prevent
            // linking incompatible architectures or ABIs.
            if let Some(ref triple) = context.target_triple {
                let triple_dir = target_dir.join(triple);
                let primary_build_root = triple_dir.join(profile).join(BUILD_DIR);
                if let Some(cache) = search_cache_in_directory(&primary_build_root, current_out_dir)
                {
                    return Some(cache);
                }

                let sibling_build_root = triple_dir.join(sibling_profile).join(BUILD_DIR);
                if let Some(cache) = search_cache_in_directory(&sibling_build_root, current_out_dir)
                {
                    return Some(cache);
                }
            } else {
                // Host native build: search host target paths
                let primary_build_root = target_dir.join(profile).join(BUILD_DIR);
                if let Some(cache) = search_cache_in_directory(&primary_build_root, current_out_dir)
                {
                    return Some(cache);
                }

                let sibling_build_root = target_dir.join(sibling_profile).join(BUILD_DIR);
                if let Some(cache) = search_cache_in_directory(&sibling_build_root, current_out_dir)
                {
                    return Some(cache);
                }

                // Also check if host target has an explicit triple folder matching TARGET
                if let Ok(host_target) = std::env::var("TARGET") {
                    let explicit_host_dir = target_dir.join(&host_target);
                    let explicit_primary = explicit_host_dir.join(profile).join(BUILD_DIR);
                    if let Some(cache) =
                        search_cache_in_directory(&explicit_primary, current_out_dir)
                    {
                        return Some(cache);
                    }
                }
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
        // src.is_dir() follows symlinks to directories (essential on macOS/Unix)
        if src.is_dir() {
            copy_directory(&src, &dst)
        } else if src.is_file() {
            fs::copy(&src, &dst).is_ok()
        } else {
            true
        }
    })
}
