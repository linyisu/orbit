use std::{env, path::PathBuf, process::Command};

pub fn bundle() {
    println!("cargo:rerun-if-env-changed=ORBIT_WORKSPACE_SERVER");
    if let Some(server) = env::var_os("ORBIT_WORKSPACE_SERVER") {
        let server = PathBuf::from(server)
            .canonicalize()
            .expect("workspace server path");
        println!("cargo:rerun-if-changed={}", server.display());
        publish(server);
        return;
    }
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target = format!("{architecture}-unknown-linux-musl");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // A separate target directory avoids waiting on the parent Cargo build lock.
    let cache = output.ancestors().nth(4).unwrap().join("workspace-server");
    let compiler_variable = format!("CC_{}", target.replace('-', "_"));
    let local_compiler = root.join("target/portable-toolchain/install/bin/musl-gcc");
    let compiler = env::var_os(&compiler_variable).unwrap_or_else(|| {
        if local_compiler.is_file() {
            local_compiler.into_os_string()
        } else {
            "musl-gcc".into()
        }
    });

    for path in ["Cargo.toml", "Cargo.lock", "crates", "vendor"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    println!("cargo:rerun-if-env-changed={compiler_variable}");

    let status = Command::new(env::var_os("CARGO").unwrap())
        .current_dir(&root)
        .args([
            "build",
            "--locked",
            "--release",
            "--bin",
            "orbit-workspace-server",
            "-p",
            "orbit-server",
            "--target",
            &target,
            "--target-dir",
        ])
        .arg(&cache)
        .args(["--jobs", "2"])
        .env(&compiler_variable, compiler)
        .env(
            format!(
                "CARGO_TARGET_{}_LINKER",
                target.replace('-', "_").to_uppercase()
            ),
            "cc",
        )
        .env(
            "CARGO_ENCODED_RUSTFLAGS",
            "-C\x1ftarget-feature=+crt-static",
        )
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .status()
        .expect("start portable workspace server build");
    assert!(
        status.success(),
        "Cannot build the embedded workspace server. Install musl-tools and run \
         `rustup target add {target}`; CC_<target> can select a musl compiler."
    );

    let server = cache.join(&target).join("release/orbit-workspace-server");
    publish(server);
}

fn publish(server: PathBuf) {
    let inspection = Command::new("readelf")
        .args(["-l", "-d"])
        .arg(&server)
        .output()
        .expect("readelf is required to verify the portable workspace server");
    let headers = String::from_utf8_lossy(&inspection.stdout);
    assert!(
        inspection.status.success() && !headers.contains("INTERP") && !headers.contains("NEEDED"),
        "The embedded workspace server must have no interpreter or shared-library dependencies"
    );
    println!("cargo:rustc-env=ORBIT_EMBEDDED_SERVER={}", server.display());
}
