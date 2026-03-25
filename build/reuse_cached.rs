use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::constant::KTX_SOFTWARE_VERSION;

const PACKAGE_PREFIX: &str = "ktx2-rw-";
const OUTPUT_DIR_NAME: &str = "out";
const KTX_BUILD_DIR_NAME: &str = "KTX-Software-build";
const KTX_BINDINGS_FILE_NAME: &str = "bindings.rs";
const LLVM_COV_TARGET_DIR: &str = "llvm-cov-target";
const BUILD_DIR: &str = "build";
const DEBUG_DIR: &str = "debug";
const RELEASE_DIR: &str = "release";

pub fn try_reuse_cached_build(current_out_dir: &Path) -> bool {
    find_cached_build(current_out_dir)
        .and_then(|cache| cache.validate().then_some(cache))
        .and_then(|cache| cache.restore_to(current_out_dir).then_some(()))
        .is_some()
}

struct CachedBuild {
    ktx_build_dir: PathBuf,
    ktx_source_dir: PathBuf,
    bindings_file: PathBuf,
}

impl CachedBuild {
    fn from_package_dir(package_dir: &Path) -> Self {
        let current_output_dir = package_dir.join(OUTPUT_DIR_NAME);
        Self {
            ktx_build_dir: current_output_dir.join(KTX_BUILD_DIR_NAME),
            ktx_source_dir: current_output_dir
                .join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION)),
            bindings_file: current_output_dir.join(KTX_BINDINGS_FILE_NAME),
        }
    }

    fn validate(&self) -> bool {
        self.ktx_build_dir.exists() && self.ktx_source_dir.exists() && self.bindings_file.exists()
    }

    fn restore_to(&self, target: &Path) -> bool {
        copy_directory(&self.ktx_build_dir, &target.join(KTX_BUILD_DIR_NAME))
            && copy_directory(
                &self.ktx_source_dir,
                &target.join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION)),
            )
            && fs::copy(&self.bindings_file, target.join(KTX_BINDINGS_FILE_NAME)).is_ok()
    }
}

fn find_cached_build(current_out_dir: &Path) -> Option<CachedBuild> {
    // current_out_dir:     project/target/debug/build/ktx2-rw-a2472af464f1a51e/out
    //                      project/target/llvm-cov-target/debug/build/ktx2-rw-a2472af464f1a51e/out
    // current_build_root:  project/target/debug/build
    //                      project/target/llvm-cov-target/debug/build
    // current_target_root: project/target
    //                      project/target/llvm-cov-target
    // cargo_target_root:   project/target
    // cargo_build_root:    project/target/debug/build

    let current_build_root = current_out_dir.parent()?.parent()?;
    let current_target_root = current_build_root.parent()?.parent()?;
    let is_llvm_cov_build = current_target_root.file_name()? == LLVM_COV_TARGET_DIR;
    let is_release = current_build_root.parent()?.file_name()? == RELEASE_DIR;
    let cargo_target_root = if is_llvm_cov_build {
        current_target_root.parent()?
    } else {
        current_target_root
    };
    let cargo_build_root = if is_llvm_cov_build {
        let cargo_profile_dir = if is_release {
            cargo_target_root.join(RELEASE_DIR)
        } else {
            cargo_target_root.join(DEBUG_DIR)
        };
        cargo_profile_dir.join(BUILD_DIR).to_path_buf()
    } else {
        current_build_root.to_path_buf()
    };

    search_cache_in_directory(cargo_build_root.as_path(), current_out_dir)
}

fn search_cache_in_directory(build_root: &Path, current_out_dir: &Path) -> Option<CachedBuild> {
    let entries = fs::read_dir(build_root).ok()?;

    entries
        .filter_map(Result::ok)
        .filter(|entry| is_candidate_package(entry.path(), current_out_dir))
        .map(|entry| CachedBuild::from_package_dir(&entry.path()))
        .find(CachedBuild::validate)
}

fn is_candidate_package(path: PathBuf, current_out_dir: &Path) -> bool {
    let file_name = match path.file_name() {
        Some(name) => name.to_string_lossy(),
        None => return false,
    };
    let parent = match current_out_dir.parent() {
        Some(p) => p,
        None => return false,
    };
    file_name.starts_with(PACKAGE_PREFIX) && path != parent
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
