use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const KTX_SOFTWARE_VERSION: &str = "4.4.0";
const KTX_SOFTWARE_URL: &str =
    "https://github.com/KhronosGroup/KTX-Software/archive/refs/tags/v4.4.0.tar.gz";
const FALLBACK_PATH: &str = "/tmp/ktx-software-v4.4.0.tar.gz";

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    // Check if we should build or use cached version
    let ktx_build_dir = out_dir.join("KTX-Software-build");
    let ktx_lib_path = ktx_build_dir.join("lib").join(get_lib_name(&target_os));

    if !ktx_lib_path.exists() {
        build_ktx_software(&out_dir, &target, &target_os, &target_arch, &target_env);
    }

    // Link the built library
    let lib_dir = ktx_build_dir.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Try to find what library was actually built and link appropriately
    if lib_dir.join("libktx.a").exists() || lib_dir.join("ktx.lib").exists() {
        println!("cargo:rustc-link-lib=static=ktx");
    } else if lib_dir.join("ktx.framework").exists() {
        // For iOS/macOS frameworks
        println!("cargo:rustc-link-search=framework={}", lib_dir.display());
        println!("cargo:rustc-link-lib=framework=ktx");
    } else if lib_dir.join("libktx.dylib").exists() || lib_dir.join("libktx.so").exists() {
        println!("cargo:rustc-link-lib=dylib=ktx");
    } else {
        // Fallback to static linking
        println!("cargo:rustc-link-lib=static=ktx");
    }

    // Link required system libraries
    link_system_libraries(&target_os, &target_env);

    // Configure bindgen
    setup_bindgen(&out_dir, &target, &ktx_build_dir);

    // Invalidation rules
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
}

fn build_ktx_software(
    out_dir: &Path,
    target: &str,
    target_os: &str,
    target_arch: &str,
    target_env: &str,
) {
    let ktx_source_dir = download_and_extract_ktx_software(out_dir);
    let ktx_build_dir = out_dir.join("KTX-Software-build");

    // Create build directory
    fs::create_dir_all(&ktx_build_dir).expect("Failed to create build directory");

    // Configure CMake
    let mut cmake_config = cmake::Config::new(&ktx_source_dir);

    // Basic configuration
    cmake_config
        .define("KTX_FEATURE_STATIC_LIBRARY", "ON")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("KTX_FEATURE_TOOLS", "OFF")
        .define("KTX_FEATURE_TESTS", "OFF")
        .define("KTX_FEATURE_LOADTEST_APPS", "OFF")
        .define("KTX_FEATURE_GL_UPLOAD", "OFF")
        .define("KTX_FEATURE_VK_UPLOAD", "OFF")
        .define("KTX_FEATURE_WRITE", "ON")
        .define("CMAKE_BUILD_TYPE", "Release")
        .out_dir(&ktx_build_dir);

    // Platform-specific configuration
    configure_cmake_for_target(
        &mut cmake_config,
        target,
        target_os,
        target_arch,
        target_env,
    );

    // Build
    let dst = cmake_config.build();

    // Find the built library - it could be static, dynamic, or framework
    let lib_dir = dst.join("lib");
    let possible_locations = [
        (lib_dir.join("libktx.a"), "libktx.a", "static"), // Static library (preferred)
        (lib_dir.join("ktx.lib"), "ktx.lib", "static"),   // Windows static library
        (lib_dir.join("libktx.dylib"), "libktx.dylib", "dylib"), // macOS dynamic library
        (lib_dir.join("libktx.so"), "libktx.so", "dylib"), // Linux dynamic library
        (
            lib_dir.join("ktx.framework").join("ktx"),
            "ktx",
            "framework",
        ), // iOS/macOS framework
    ];

    let mut found_lib = None;
    for (lib_path, lib_name, lib_type) in &possible_locations {
        if lib_path.exists() {
            found_lib = Some((lib_path.clone(), lib_name.to_string(), lib_type.to_string()));
            break;
        }
    }

    let (_, lib_name, lib_type) = found_lib.expect("No KTX library found after build");

    // Handle different library types - only warn for dynamic libraries
    if matches!(lib_type.as_str(), "dylib") {
        println!("cargo:warning=Built dynamic library {}, you may need to set LD_LIBRARY_PATH or DYLD_LIBRARY_PATH", lib_name);
    }
}

fn download_and_extract_ktx_software(out_dir: &Path) -> PathBuf {
    let ktx_source_dir = out_dir.join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION));

    // Skip download if already exists
    if ktx_source_dir.exists() {
        return ktx_source_dir;
    }

    // Only print download message when actually downloading

    // Create client with longer timeout
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("Failed to create HTTP client");

    // Try fallback path first
    if std::path::Path::new(FALLBACK_PATH).exists() {
        let tar_gz_data = std::fs::read(FALLBACK_PATH).expect("Failed to read fallback archive");
        let tar_gz = flate2::read::GzDecoder::new(&tar_gz_data[..]);
        let mut archive = tar::Archive::new(tar_gz);
        archive
            .unpack(out_dir)
            .expect("Failed to extract KTX-Software");
        return ktx_source_dir;
    }

    // Download with retries
    let mut last_error = None;
    for attempt in 1..=3 {
        match client.get(KTX_SOFTWARE_URL).send() {
            Ok(response) => {
                match response.bytes() {
                    Ok(bytes) => {
                        // Extract tar.gz
                        let tar_gz = flate2::read::GzDecoder::new(&bytes[..]);
                        let mut archive = tar::Archive::new(tar_gz);
                        archive
                            .unpack(out_dir)
                            .expect("Failed to extract KTX-Software");

                        // Download successful
                        return ktx_source_dir;
                    }
                    Err(e) => {
                        last_error = Some(format!("Failed to read response body: {}", e));
                        if attempt < 3 {
                            std::thread::sleep(std::time::Duration::from_secs(5));
                        }
                    }
                }
            }
            Err(e) => {
                last_error = Some(format!("Failed to download KTX-Software: {}", e));
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            }
        }
    }

    panic!(
        "Download failed: {}. You can manually download {} and place it at {} to use as fallback",
        last_error.unwrap_or_else(|| "Unknown download error".to_string()),
        KTX_SOFTWARE_URL,
        FALLBACK_PATH
    );
}

fn configure_cmake_for_target(
    cmake_config: &mut cmake::Config,
    target: &str,
    target_os: &str,
    target_arch: &str,
    target_env: &str,
) {
    match target_os {
        "windows" => {
            cmake_config.define("CMAKE_SYSTEM_NAME", "Windows");
            if target_env == "gnu" {
                if target.contains("x86_64") {
                    cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "x86_64");
                } else if target.contains("i686") {
                    cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "i686");
                }

                // When cross-compiling from non-Windows, use Unix Makefiles instead of MinGW Makefiles
                if !cfg!(windows) {
                    cmake_config.generator("Unix Makefiles");
                } else {
                    cmake_config.generator("MinGW Makefiles");
                }
            } else {
                // For MSVC targets, only use Visual Studio generator on Windows host
                if cfg!(windows) {
                    cmake_config.generator("Visual Studio 17 2022");
                    if target_arch == "x86_64" {
                        cmake_config.define("CMAKE_GENERATOR_PLATFORM", "x64");
                    } else if target_arch == "aarch64" {
                        cmake_config.define("CMAKE_GENERATOR_PLATFORM", "ARM64");
                    }

                    // Use KTX-Software's official MSVC configuration approach
                    // Mimic their Windows CI build exactly

                    // Add Basis Universal SSE support like their CI
                    cmake_config.define("BASISU_SUPPORT_SSE", "ON");
                } else {
                    // Cross-compiling MSVC from non-Windows is not supported
                    panic!("Cross-compiling to Windows MSVC targets from non-Windows platforms is not supported. Use GNU targets instead (e.g., x86_64-pc-windows-gnu)");
                }
            }
        }
        "macos" => {
            cmake_config.define("CMAKE_SYSTEM_NAME", "Darwin");
            if target_arch == "aarch64" {
                cmake_config.define("CMAKE_OSX_ARCHITECTURES", "arm64");
                cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "arm64");
            } else {
                cmake_config.define("CMAKE_OSX_ARCHITECTURES", "x86_64");
                cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "x86_64");
            }
        }
        "linux" => {
            cmake_config.define("CMAKE_SYSTEM_NAME", "Linux");
            cmake_config.define("CMAKE_SYSTEM_PROCESSOR", target_arch);

            // Handle musl vs glibc
            if target_env == "musl" {
                cmake_config.define("CMAKE_C_FLAGS", "-static");
                cmake_config.define("CMAKE_CXX_FLAGS", "-static");

                // Set musl compilers explicitly
                // The cmake crate tries to find x86_64-linux-musl-g++ but it doesn't exist
                // We need to use the standard gcc/g++ with musl
                cmake_config.define("CMAKE_C_COMPILER", "gcc");
                cmake_config.define("CMAKE_CXX_COMPILER", "g++");
            }
        }
        "android" => {
            cmake_config.define("CMAKE_SYSTEM_NAME", "Android");
            cmake_config.define("CMAKE_ANDROID_API", "21");
            cmake_config.define(
                "CMAKE_ANDROID_ARCH_ABI",
                match target_arch {
                    "aarch64" => "arm64-v8a",
                    "arm" => "armeabi-v7a",
                    "x86_64" => "x86_64",
                    "i686" => "x86",
                    _ => target_arch,
                },
            );

            if let Ok(ndk_path) = env::var("ANDROID_NDK_ROOT") {
                cmake_config.define(
                    "CMAKE_TOOLCHAIN_FILE",
                    format!("{}/build/cmake/android.toolchain.cmake", ndk_path),
                );
            }
        }
        "ios" => {
            cmake_config.define("CMAKE_SYSTEM_NAME", "iOS");
            cmake_config.define("CMAKE_OSX_DEPLOYMENT_TARGET", "11.0");
            if target_arch == "aarch64" {
                cmake_config.define("CMAKE_OSX_ARCHITECTURES", "arm64");
            } else {
                cmake_config.define("CMAKE_OSX_ARCHITECTURES", "x86_64");
            }
        }
        _ => {
            // Generic Unix-like system
            cmake_config.define("CMAKE_SYSTEM_PROCESSOR", target_arch);
        }
    }
}

fn get_lib_name(target_os: &str) -> &'static str {
    match target_os {
        "windows" => "ktx.lib",
        _ => "libktx.a",
    }
}

fn link_system_libraries(target_os: &str, target_env: &str) {
    match target_os {
        "macos" => {
            println!("cargo:rustc-link-lib=c++");
        }
        "linux" => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=m");
            println!("cargo:rustc-link-lib=dl");
            println!("cargo:rustc-link-lib=pthread");
        }
        "windows" => {
            if target_env == "gnu" {
                // MinGW/GNU environment
                println!("cargo:rustc-link-lib=stdc++");
                println!("cargo:rustc-link-lib=gcc_s");
                println!("cargo:rustc-link-lib=gcc");
                println!("cargo:rustc-link-lib=pthread");
                println!("cargo:rustc-link-lib=ssp");
                println!("cargo:rustc-link-lib=mingw32");
            } else {
                // MSVC environment - use clean linking like KTX-Software CI
                // No additional system libraries needed - let MSVC handle CRT automatically
            }
        }
        "android" => {
            println!("cargo:rustc-link-lib=c++");
            println!("cargo:rustc-link-lib=m");
            println!("cargo:rustc-link-lib=dl");
        }
        "ios" => {
            println!("cargo:rustc-link-lib=c++");
        }
        _ => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=m");
            println!("cargo:rustc-link-lib=dl");
            println!("cargo:rustc-link-lib=pthread");
        }
    }
}

fn setup_bindgen(out_dir: &Path, target: &str, _ktx_build_dir: &Path) {
    let ktx_source_dir = out_dir.join(format!("KTX-Software-{}", KTX_SOFTWARE_VERSION));
    let header_path = ktx_source_dir.join("include").join("ktx.h");

    let mut builder = bindgen::Builder::default()
        .header(header_path.to_string_lossy())
        .clang_arg(format!("-I{}", ktx_source_dir.join("include").display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // Add target-specific configuration for cross-compilation
    if target.contains("windows") && !cfg!(windows) {
        // Cross-compiling to Windows
        if let Ok(mingw_path) = env::var("MINGW_PREFIX") {
            builder = builder
                .clang_arg(format!("-I{}/include", mingw_path))
                .clang_arg(format!("--sysroot={}", mingw_path));
        } else if cfg!(target_os = "macos") {
            // Use system MinGW installation on macOS
            let mingw_sysroot = if target.contains("x86_64") {
                "/opt/homebrew/opt/mingw-w64/toolchain-x86_64/x86_64-w64-mingw32"
            } else {
                "/opt/homebrew/opt/mingw-w64/toolchain-i686/i686-w64-mingw32"
            };

            builder = builder
                .clang_arg("-target")
                .clang_arg(target)
                .clang_arg(format!("--sysroot={}", mingw_sysroot))
                .clang_arg(format!("-I{}/include", mingw_sysroot));
        }
    } else if target.contains("android") {
        // Cross-compiling to Android
        if let Ok(ndk_path) = env::var("ANDROID_NDK_ROOT") {
            let android_api = "21"; // Minimum API level we're targeting
            let arch_triple = if target.contains("aarch64") {
                "aarch64-linux-android"
            } else if target.contains("armv7") || target.contains("arm") {
                "arm-linux-androideabi"
            } else if target.contains("x86_64") {
                "x86_64-linux-android"
            } else if target.contains("i686") {
                "i686-linux-android"
            } else {
                target
            };

            // Detect the host platform for the NDK prebuilt toolchain
            let host_tag = if cfg!(target_os = "macos") {
                "darwin-x86_64"
            } else if cfg!(target_os = "linux") {
                "linux-x86_64"
            } else if cfg!(target_os = "windows") {
                "windows-x86_64"
            } else {
                "linux-x86_64" // fallback
            };

            let sysroot = format!("{}/toolchains/llvm/prebuilt/{}/sysroot", ndk_path, host_tag);

            builder = builder
                .clang_arg("-target")
                .clang_arg(format!("{}{}", arch_triple, android_api))
                .clang_arg(format!("--sysroot={}", sysroot))
                .clang_arg(format!("-I{}/usr/include", sysroot))
                .clang_arg(format!("-I{}/usr/include/{}", sysroot, arch_triple))
                .clang_arg("-DANDROID");
        }
    }

    // Generate the bindings
    let bindings = builder.generate().expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
