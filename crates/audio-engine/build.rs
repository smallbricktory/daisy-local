fn main() {
    #[cfg(target_os = "macos")]
    {
        use std::path::PathBuf;
        let out = std::env::var("OUT_DIR").unwrap();
        let shim = "src/macos/shim.swift";
        println!("cargo:rerun-if-changed={shim}");

        let lib = PathBuf::from(&out).join("libdaisysck.a");
        let status = std::process::Command::new("swiftc")
            .args([
                "-emit-library",
                "-static",
                "-O",
                "-target",
                "arm64-apple-macosx15.0",
                "-module-name",
                "daisysck",
                "-o",
                lib.to_str().unwrap(),
                shim,
            ])
            .status()
            .expect("run swiftc");
        assert!(status.success(), "swiftc failed building shim.swift");

        println!("cargo:rustc-link-search=native={out}");
        println!("cargo:rustc-link-lib=static=daisysck");
        for fw in [
            "AVFoundation",
            "AudioToolbox",
            "CoreAudio",
            "Foundation",
        ] {
            println!("cargo:rustc-link-lib=framework={fw}");
        }
        // Swift OS runtime (ABI-stable, OS-provided at macOS 15+).
        println!("cargo:rustc-link-search=native=/usr/lib/swift");
        println!("cargo:rustc-link-lib=dylib=swiftCore");
    }
}
