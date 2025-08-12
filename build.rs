use std::env;
use std::path::PathBuf;

fn main() {
    // Tell cargo to tell rustc to link the ktx2 library
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let lib_name = match (
        target_os.as_str(),
        target_arch.as_str(),
        target_env.as_str(),
    ) {
        ("linux", "x86_64", "musl") => "ktx2-linux-x64-musl",
        ("linux", "x86_64", _) => "ktx2-linux-x64-glibc", // default to glibc for linux
        ("macos", "x86_64", _) => "ktx2-macos-x64",
        ("macos", "aarch64", _) => "ktx2-macos-aarch64",
        ("windows", "x86_64", "gnu") => "ktx2-windows-x64-gnu",
        ("windows", "x86_64", _) => "ktx2-windows-x64", // default to MSVC for windows
        _ => panic!("Unsupported platform: {target_os}-{target_arch}-{target_env}"),
    };

    // Add the library search path
    println!("cargo:rustc-link-search=native=libktx2-sys/lib");

    // Link the platform-specific static library
    println!("cargo:rustc-link-lib=static={lib_name}");

    // Link required system libraries for C++
    match target_os.as_str() {
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
            let target = env::var("TARGET").unwrap_or_default();
            if target.contains("gnu") {
                // For Windows GNU (MinGW) targets
                // The library might have been compiled with libc++ but we need to link with available libs
                println!("cargo:rustc-link-lib=stdc++");
                println!("cargo:rustc-link-lib=gcc_s");
                println!("cargo:rustc-link-lib=gcc");
                // Add pthread and additional Windows libraries
                println!("cargo:rustc-link-lib=pthread");
                println!("cargo:rustc-link-lib=ssp");
                println!("cargo:rustc-link-lib=mingw32");
            } else {
                // For Windows MSVC targets
                println!("cargo:rustc-link-lib=msvcrt");
            }
        }
        _ => {}
    }

    // Tell cargo to invalidate the built crate whenever the wrapper changes
    println!("cargo:rerun-if-changed=libktx2-sys/include/ktx.h");

    // Configure bindgen for cross-compilation
    let target = env::var("TARGET").unwrap();
    let mut builder = bindgen::Builder::default()
        .header("libktx2-sys/include/ktx.h")
        .clang_arg("-Ilibktx2-sys/include")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // Add target-specific configuration for cross-compilation
    if target.contains("windows") && !cfg!(windows) {
        // Only apply special configuration when cross-compiling to Windows from another OS
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
                .clang_arg(&target)
                .clang_arg(format!("--sysroot={}", mingw_sysroot))
                .clang_arg(format!("-I{}/include", mingw_sysroot));
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
