fn main() {
    // Relinks the binary when the embedded frontend dist changes.
    println!("cargo:rerun-if-changed=../../apps/frontend/dist");
    println!("cargo:rerun-if-changed=../../apps/frontend/dist/index.html");
    println!("cargo:rerun-if-changed=../../apps/frontend/dist/assets");

    // Stamps the build id: the DAISY_BUILD_SHA env var if set, else the git
    // short SHA, else "unknown".
    println!("cargo:rerun-if-env-changed=DAISY_BUILD_SHA");
    let sha = std::env::var("DAISY_BUILD_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".into())
        });
    println!("cargo:rustc-env=DAISY_BUILD_SHA={sha}");
    // HEAD only names the branch; the branch ref (or packed-refs) carries the
    // commit that same-branch commits actually change.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");

    // Stamps whether this is a tagged release build: the DAISY_BUILD_TAGGED
    // env var if set (release.yml sets it to 1), else 1 when HEAD is exactly
    // a v* tag, else 0. Untagged builds show the testing-only banner in
    // Settings → About.
    println!("cargo:rerun-if-env-changed=DAISY_BUILD_TAGGED");
    let tagged = match std::env::var("DAISY_BUILD_TAGGED") {
        Ok(v) if !v.trim().is_empty() => v.trim() == "1",
        _ => std::process::Command::new("git")
            .args(["describe", "--exact-match", "--tags", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with('v'))
            .unwrap_or(false),
    };
    println!(
        "cargo:rustc-env=DAISY_BUILD_TAGGED={}",
        if tagged { "1" } else { "0" }
    );

    // Stamps the build time, compared against the update manifest's
    // `released_at_unix`. DAISY_BUILD_UNIX env overrides.
    println!("cargo:rerun-if-env-changed=DAISY_BUILD_UNIX");
    let built = std::env::var("DAISY_BUILD_UNIX")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    println!("cargo:rustc-env=DAISY_BUILD_UNIX={built}");

    tauri_build::build();
}
