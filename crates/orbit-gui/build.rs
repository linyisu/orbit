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
            const WINDOWS_GUI_STACK_BYTES: usize = 8 * 1_024 * 1_024;
            // Debug GPUI view construction has large finite stack frames. Match
            // the desktop stack budget instead of the MSVC 1 MiB default.
            let stack_arg = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
                format!("/STACK:{WINDOWS_GUI_STACK_BYTES}")
            } else {
                format!("-Wl,--stack,{WINDOWS_GUI_STACK_BYTES}")
            };
            println!("cargo:rustc-link-arg-bin=orbit-gui={stack_arg}");
            embed_resource::compile_for("resources/orbit.rc", ["orbit-gui"], embed_resource::NONE)
                .manifest_required()
                .expect("compile Orbit desktop resources");
        }
        _ => {}
    }
}
