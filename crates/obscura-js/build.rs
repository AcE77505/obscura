use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(obscura_external_bootstrap)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=OBSCURA_SNAPSHOT_FILE");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let snapshot_path = out_dir.join("OBSCURA_SNAPSHOT.bin");

    // V8 快照与架构强绑定；交叉编译时不能由 host V8 生成，改为注入目标架构的快照。
    let snapshot_file = std::env::var("OBSCURA_SNAPSHOT_FILE")
        .expect("OBSCURA_SNAPSHOT_FILE must be set to an architecture-correct V8 snapshot");
    std::fs::copy(&snapshot_file, &snapshot_path).expect("Failed to copy OBSCURA_SNAPSHOT_FILE");

    println!("cargo:rustc-cfg=obscura_external_bootstrap");
    println!("cargo:rustc-env=OBSCURA_SNAPSHOT_PATH={}", snapshot_path.display());
}
