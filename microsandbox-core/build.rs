use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Only link against libkrun on Unix platforms
    if target_os != "windows" {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let build_dir = std::path::Path::new(&manifest_dir)
            .parent()
            .unwrap()
            .join("build");

        // Add build directory as first search path
        println!("cargo:rustc-link-search=native={}", build_dir.display());

        // Add system paths as fallback
        println!("cargo:rustc-link-search=native=/usr/local/lib");

        // Add user-specific library as fallback
        if let Ok(home) = env::var("HOME") {
            println!("cargo:rustc-link-search=native={}/.local/lib", home);
        }

        // Link against libkrun library
        println!("cargo:rustc-link-lib=dylib=krun");

        // Force rebuild if the library changes
        println!(
            "cargo:rerun-if-changed={}",
            build_dir.join("libkrun.dylib").display()
        );

        println!(
            "cargo:rerun-if-changed={}",
            build_dir.join("libkrun.so").display()
        );
    }
}
