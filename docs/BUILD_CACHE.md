# Build Artifact Cache & Worktree Sharing

This document describes the pre-compiled C library (`KTX-Software`) cache reuse system in `ktx2-rw`, its multi-worktree discovery mechanism, architecture safety guarantees, and configuration options.

---

## Background

`ktx2-rw` builds the underlying C/C++ [KTX-Software](https://github.com/KhronosGroup/KTX-Software) library from source at compile time via CMake. A fresh compilation involves:
1. Downloading and unpacking source dependencies.
2. Compiling the Basis Universal encoder/transcoder and KTX core libraries with C++ compilers.
3. Generating Rust bindings via `bindgen`.

On a typical workstation, this takes between **5 to 10 minutes**. To optimize developer velocity and CI efficiency, `ktx2-rw` implements an automatic, cross-platform build artifact cache reuse mechanism in its build script (`build/reuse_cached.rs`).

---

## How It Works

During `cargo build`, before invoking CMake or downloading sources, `ktx2-rw` checks whether a valid, matching build of `KTX-Software` already exists on the local machine:

```
                  ┌───────────────────────────────┐
                  │      cargo build begins       │
                  └───────────────┬───────────────┘
                                  ▼
                     KTX_NO_CACHE_REUSE set?
                     ├── Yes ──► Clean Build (CMake)
                     └── No
                          ▼
            Phase 1: Local Target Directory
            (Check current target/ for sibling profiles)
                     ├── Found ──► Validate & Reuse
                     └── Not Found
                          ▼
             Phase 2: Git Worktree Discovery
            (Discover sibling worktrees via git CLI
             or filesystem .git pointers)
                     ├── Found worktrees?
                     │    ▼
                     │    Scan candidate target dirs
                     │    (target/, target-*/)
                     │    ├── Found ──► Validate & Reuse
                     │    └── Not Found
                     └── No / Disabled (KTX_NO_WORKTREE_CACHE)
                          ▼
                  Clean Build (CMake)
```

### 1. Local Target Directory Search
The build script first inspects the `build/` folder within the current `CARGO_TARGET_DIR`. It checks whether another profile (e.g. `debug` or `release`) or a previous compilation produced a valid artifact.

### 2. Git Worktree Discovery
When working on large repositories with multiple Git worktrees (e.g., `git worktree add ../feature-branch`), developers traditionally had to re-compile all C dependencies in each worktree's separate target directory.

`ktx2-rw` automatically detects sibling worktrees by:
- Querying `git worktree list --porcelain` from the consumer crate's repository root.
- Gracefully falling back to parsing the `.git` file (which points to the common gitdir) if the `git` binary is not in `PATH`.
- Gracefully skipping worktree discovery if the consuming project is not a Git repository.

### 3. Candidate Target Directories
In multi-worktree environments, developers frequently assign independent target directories (such as `target/`, `target-debug/`, or `target-<task_name>/`) to avoid file locks and artifact overwrites. `ktx2-rw` scans both standard `target` and glob-matched `target-*` directories across sibling worktrees.

---

## Safety & Isolation Guarantees

Cache reuse operates under strict isolation rules to prevent corrupted builds or binary incompatibility:

### 1. Target Triple & Architecture Isolation
When cross-compiling (e.g. `--target aarch64-linux-android` or `--target x86_64-unknown-linux-gnu`), Cargo places build outputs inside `target/<TARGET_TRIPLE>/<PROFILE>/build`.

`ktx2-rw` extracts and enforces the target triple on all cache lookups:
- An `x86_64` host build will **never** be reused for an `aarch64` or `armv7` target.
- Host builds only match host builds; cross-compilation targets only match the exact same triple.

### 2. Operating System Binary Validation
Before any cache is declared reusable, `ktx2-rw` dynamically verifies the existence, readability, and non-zero size of the expected platform library:
- **Windows**: `ktx.lib` or `libktx.a`
- **macOS / iOS**: `libktx.a`, `libktx.dylib`, or `ktx.framework`
- **Linux / Android / Unix**: `libktx.a` or `libktx.so`

It also ensures that `bindings.rs` is present and valid.

### 3. Lean Copying (75%+ Disk Reduction)
CMake generates substantial intermediate build files (`.obj`, `.vcxproj`, `CMakeCache.txt`, compiler logs) exceeding 30MB per build.

`ktx2-rw` selectively copies only the actual distributables:
- `KTX-Software-build/lib/` (the compiled static/dynamic libraries)
- `KTX-Software-build/include/` (the C headers required for linking and bindings)
- `bindings.rs` (pre-generated Rust FFI bindings)

This reduces the copy payload from ~35MB to ~3MB, accelerates restoration to sub-second speeds, and avoids macOS symlink issues with `.framework/Headers`.

### 4. Atomic Copy with Rollback
If an error occurs midway through copying files (e.g. disk full, permission denied), the partial destination directory is automatically cleaned up and removed to prevent leaving corrupted or half-baked artifacts.

### 5. Path Canonicalization
Paths are normalized with slash consistency and Windows case-insensitivity checks (e.g., matching `D:\repo` and `d:\repo`), preventing false negatives and cyclic self-copies.

---

## Environment Variables & Configuration

The build process can be controlled via environment variables:

| Variable | Values | Default | Description |
| :--- | :--- | :---: | :--- |
| `KTX_NO_CACHE_REUSE` | `1`, `true` | `0` | Completely bypasses cache reuse and forces a clean build from source via CMake. Useful in CI or when debugging C library modifications. |
| `KTX_NO_WORKTREE_CACHE` | `1`, `true` | `0` | Disables searching across sibling Git worktrees. Cache reuse will still occur within the current target directory. |
| `KTX_FEATURE_SSE` | `0`, `off`, `false`, `no` | `on` | Disables SSE optimizations for the Basis Universal encoder (useful if compiler flags conflict with SSE intrinsics). |

---

## Common Use Cases

### 1. Daily Development with Multiple Worktrees (Zero Configuration)
No action needed. When you create a new worktree and run `cargo build`, `ktx2-rw` will automatically detect that another worktree already built `KTX-Software`, copy the precompiled library and bindings in seconds, and proceed directly to compiling the Rust crate.

You will see the following cargo warning in the build output confirming the reuse:
```text
cargo:warning=ktx2-rw: Successfully reused cached build artifacts from /path/to/sibling/target/...
```

### 2. Clean CI / Release Builds
For reproducible release builds or CI matrix jobs where every artifact must be built from scratch:
```bash
KTX_NO_CACHE_REUSE=1 cargo build --release
```

### 3. Confining Builds to the Current Target
If you want to allow intra-target cache sharing (e.g. between debug and release within the same worktree) but prohibit cross-worktree discovery:
```bash
KTX_NO_WORKTREE_CACHE=1 cargo build
```
