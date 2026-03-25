use std::env;
use std::path::Path;

fn main() {
    built::write_built_file().expect("Failed to acquire build-time information");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BILI_SYNC_RELEASE_CHANNEL");
    track_frontend_build_output();

    if let Ok(value) = env::var("BILI_SYNC_RELEASE_CHANNEL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            println!("cargo:rustc-env=BILI_SYNC_RELEASE_CHANNEL_BUILT={}", trimmed);
        }
    }
}

fn track_frontend_build_output() {
    let candidates = [
        "../../web/build/index.html",
        "../../web/build/_app/version.json",
        "../../web/build/manifest.webmanifest",
        "../../web/build/sw.js",
        "../../web/build/registerSW.js",
    ];

    let mut any_exists = false;
    for rel in candidates {
        let path = Path::new(rel);
        if path.exists() {
            any_exists = true;
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    if !any_exists {
        println!("cargo:warning=未找到前端构建产物(web/build)。如需更新管理界面，请先执行: cd web && npm run build");
    }
}
