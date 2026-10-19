fn main() {
    orbit_build::embed_workspace_server();
    println!("cargo:rerun-if-changed=resources");
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            let plist =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/usage.plist");
            println!(
                "cargo:rustc-link-arg-bin=orbit-gui=-Wl,-sectcreate,__TEXT,__info_plist,{}",
                plist.display()
            );
        }
        Ok("windows") => {
            embed_resource::compile_for("resources/orbit.rc", ["orbit-gui"], embed_resource::NONE)
                .manifest_required()
                .expect("compile Orbit desktop resources");
        }
        _ => {}
    }
}
