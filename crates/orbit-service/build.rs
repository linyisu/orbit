use std::{env, path::PathBuf, time::SystemTime};

fn main() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    for path in ["Cargo.toml", "Cargo.lock", "vendor"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    for package in [
        "orbit-server",
        "orbit-service",
        "orbit-protocol",
        "orbit-config",
        "orbit-agent",
        "orbit-auth",
        "orbit-model",
        "orbit-runtime",
        "orbit-store",
        "orbit-tools",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            root.join("crates").join(package).display()
        );
    }
    // Release builders can supply a reproducible epoch. Local builds advance
    // when any server source changes, including tools linked into the daemon.
    let generation = env::var("SOURCE_DATE_EPOCH")
        .map(|epoch| {
            epoch
                .parse::<u64>()
                .expect("SOURCE_DATE_EPOCH must be seconds")
                .checked_mul(1000)
                .expect("SOURCE_DATE_EPOCH overflow")
        })
        .unwrap_or_else(|_| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                .try_into()
                .unwrap()
        });
    println!("cargo:rustc-env=ORBIT_SERVICE_BUILD_GENERATION={generation}");
}
