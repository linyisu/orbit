//! Shared build-time support for embedding the portable workspace server in clients.
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// Build or locate a native server and expose its verified path to the client's build.
/// Linux retains the musl deployment asset; macOS and Windows embed their native executable.
pub fn embed_workspace_server() {
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let operating_system = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target = server_target(
        &architecture,
        &operating_system,
        &env::var("TARGET").unwrap(),
    )
    .expect("supported native server target");
    println!("cargo:rerun-if-env-changed=ORBIT_WORKSPACE_SERVER");
    if let Some(server) = env::var_os("ORBIT_WORKSPACE_SERVER") {
        let server = PathBuf::from(server)
            .canonicalize()
            .expect("workspace server path");
        println!("cargo:rerun-if-changed={}", server.display());
        publish(server, &target);
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // A separate target directory avoids waiting on the parent Cargo build lock.
    let cache = output
        .ancestors()
        .nth(4)
        .unwrap()
        .join("workspace-server");
    let compiler_variable = format!("CC_{}", target.replace('-', "_"));
    let local_compiler = root.join("target/portable-toolchain/install/bin/musl-gcc");
    let compiler = env::var_os(&compiler_variable).unwrap_or_else(|| {
        if local_compiler.is_file() {
            local_compiler.into_os_string()
        } else {
            "musl-gcc".into()
        }
    });

    for path in ["Cargo.toml", "Cargo.lock", "crates/orbit-build", "vendor"] {
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
        "orbit-tool-macros",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            root.join("crates")
                .join(package)
                .display()
        );
    }
    println!("cargo:rerun-if-env-changed={compiler_variable}");

    let mut command = Command::new(env::var_os("CARGO").unwrap());
    command
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
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if operating_system == "linux" {
        command
            .env(&compiler_variable, compiler)
            .env(
                format!(
                    "CARGO_TARGET_{}_LINKER",
                    target.replace('-', "_").to_uppercase()
                ),
                "cc",
            );
    }
    if operating_system == "linux" || operating_system == "windows" {
        command.env(
            "CARGO_ENCODED_RUSTFLAGS",
            "-C\x1ftarget-feature=+crt-static",
        );
    }
    let status = command
        .status()
        .expect("start native workspace server build");
    assert!(
        status.success(),
        "Cannot build the embedded workspace server for {target}. Install the native target \
         toolchain; Linux additionally requires musl-tools and the matching musl Rust target."
    );

    let server = cache
        .join(&target)
        .join("release")
        .join(if operating_system == "windows" {
            "orbit-workspace-server.exe"
        } else {
            "orbit-workspace-server"
        });
    publish(server, &target);
}

fn publish(server: PathBuf, target: &str) {
    verify_native_server(&server, target).expect("native workspace server verification");
    println!("cargo:rustc-env=ORBIT_EMBEDDED_SERVER={}", server.display());
    println!("cargo:rerun-if-env-changed=ORBIT_REMOTE_WORKSPACE_SERVER");
    let remote = if let Some(remote) = env::var_os("ORBIT_REMOTE_WORKSPACE_SERVER") {
        let remote = PathBuf::from(remote)
            .canonicalize()
            .expect("remote server path");
        let architecture = target
            .split('-')
            .next()
            .expect("target architecture");
        verify_native_server(&remote, &format!("{architecture}-unknown-linux-musl"))
            .expect("static remote workspace server verification");
        println!("cargo:rerun-if-changed={}", remote.display());
        remote
    } else if target.contains("-linux-") {
        server
    } else {
        let empty = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("no-remote-server");
        std::fs::write(&empty, []).expect("empty remote asset marker");
        empty
    };
    println!(
        "cargo:rustc-env=ORBIT_EMBEDDED_REMOTE_SERVER={}",
        remote.display()
    );
}

/// Check that a server can start without a system ELF loader or shared libraries.
pub fn verify_static_server(server: &Path) -> Result<(), String> {
    let bytes = std::fs::read(server).map_err(|error| error.to_string())?;
    verify_static_bytes(&bytes)
}

fn verify_static_bytes(bytes: &[u8]) -> Result<(), String> {
    match object::File::parse(bytes).map_err(|error| error.to_string())? {
        object::File::Elf32(file) => verify_elf_segments(&file),
        object::File::Elf64(file) => verify_elf_segments(&file),
        _ => Err("remote server must be ELF".into()),
    }
}

fn verify_elf_segments<'data, Elf: object::read::elf::FileHeader>(
    file: &object::read::elf::ElfFile<'data, Elf>,
) -> Result<(), String> {
    use object::read::elf::{Dyn, ProgramHeader};
    let endian = file.endian();
    for segment in file.elf_program_headers() {
        let dynamic = segment
            .dynamic(endian, file.data())
            .map_err(|error| error.to_string())?;
        if segment.p_type(endian) == object::elf::PT_INTERP
            || dynamic.is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.tag32(endian) == Some(object::elf::DT_NEEDED))
            })
        {
            return Err("The embedded workspace server must have no interpreter or shared-library dependencies".into());
        }
    }
    Ok(())
}

fn server_target(
    architecture: &str,
    operating_system: &str,
    native: &str,
) -> Result<String, String> {
    match operating_system {
        "linux" => Ok(format!("{architecture}-unknown-linux-musl")),
        "macos" if native.ends_with("-apple-darwin") => Ok(native.into()),
        "windows" if native.ends_with("-pc-windows-msvc") => Ok(native.into()),
        _ => Err(format!("unsupported native server target: {native}")),
    }
}

/// Verify format, CPU architecture and platform dependency constraints without executing the asset.
pub fn verify_native_server(server: &Path, target: &str) -> Result<(), String> {
    use object::{Architecture, BinaryFormat, Object, ObjectKind};
    let bytes = std::fs::read(server).map_err(|error| error.to_string())?;
    let file = object::File::parse(bytes.as_slice()).map_err(|error| error.to_string())?;
    let architecture = if target.starts_with("x86_64-") {
        Architecture::X86_64
    } else if target.starts_with("aarch64-") {
        Architecture::Aarch64
    } else {
        return Err(format!("unsupported server architecture: {target}"));
    };
    let format = if target.contains("-linux-") {
        BinaryFormat::Elf
    } else if target.ends_with("-apple-darwin") {
        BinaryFormat::MachO
    } else if target.ends_with("-windows-msvc") {
        BinaryFormat::Pe
    } else {
        return Err(format!("unsupported server format: {target}"));
    };
    if file.architecture() != architecture
        || file.format() != format
        || !(file.kind() == ObjectKind::Executable
            || (format == BinaryFormat::Elf && file.kind() == ObjectKind::Dynamic))
    {
        return Err(format!("workspace server does not match {target}"));
    }
    if format == BinaryFormat::Elf {
        return verify_static_server(server);
    }
    for import in file
        .imports()
        .map_err(|error| error.to_string())?
    {
        let library = std::str::from_utf8(import.library()).map_err(|error| error.to_string())?;
        if !system_library(format, library) {
            return Err(format!(
                "workspace server requires an unbundled library: {library}"
            ));
        }
    }
    Ok(())
}

fn system_library(format: object::BinaryFormat, library: &str) -> bool {
    match format {
        object::BinaryFormat::MachO => {
            library.starts_with("/usr/lib/") || library.starts_with("/System/Library/")
        }
        object::BinaryFormat::Pe => {
            let library = library.to_ascii_lowercase();
            !library.contains(['/', '\\'])
                && (matches!(
                    library.as_str(),
                    "kernel32.dll"
                        | "kernelbase.dll"
                        | "ntdll.dll"
                        | "advapi32.dll"
                        | "bcrypt.dll"
                        | "bcryptprimitives.dll"
                        | "crypt32.dll"
                        | "ncrypt.dll"
                        | "secur32.dll"
                        | "sspicli.dll"
                        | "ws2_32.dll"
                        | "iphlpapi.dll"
                        | "userenv.dll"
                        | "user32.dll"
                        | "shell32.dll"
                        | "ole32.dll"
                        | "oleaut32.dll"
                        | "rpcrt4.dll"
                        | "normaliz.dll"
                        | "dnsapi.dll"
                        | "msvcrt.dll"
                        | "ucrtbase.dll"
                        | "dbghelp.dll"
                ) || library.starts_with("api-ms-win-")
                    || library.starts_with("ext-ms-win-"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf_segment(kind: u32, dynamic_tag: u64) -> Vec<u8> {
        let mut bytes = vec![0_u8; 136];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&kind.to_le_bytes());
        bytes[72..80].copy_from_slice(&120_u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&16_u64.to_le_bytes());
        bytes[120..128].copy_from_slice(&dynamic_tag.to_le_bytes());
        bytes
    }

    #[test]
    fn static_validation_rejects_loader_and_dynamic_dependencies_without_external_tools() {
        assert!(verify_static_bytes(&elf_segment(object::elf::PT_NULL, 0)).is_ok());
        assert!(verify_static_bytes(&elf_segment(object::elf::PT_INTERP, 0)).is_err());
        assert!(verify_static_bytes(&elf_segment(object::elf::PT_DYNAMIC, 0)).is_ok());
        assert!(
            verify_static_bytes(&elf_segment(
                object::elf::PT_DYNAMIC,
                u64::from(object::elf::DT_NEEDED)
            ))
            .is_err()
        );
        assert!(verify_static_bytes(b"invalid executable").is_err());
    }

    #[test]
    fn native_target_selection_keeps_linux_static_and_other_platforms_native() {
        assert_eq!(
            server_target("x86_64", "linux", "x86_64-unknown-linux-gnu").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            server_target("aarch64", "macos", "aarch64-apple-darwin").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            server_target("x86_64", "windows", "x86_64-pc-windows-msvc").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert!(server_target("x86_64", "windows", "x86_64-pc-windows-gnu").is_err());
    }

    #[test]
    fn bundle_dependencies_exclude_non_system_runtime_libraries() {
        use object::BinaryFormat::{MachO, Pe};
        assert!(system_library(Pe, "KERNEL32.dll"));
        assert!(!system_library(Pe, "VCRUNTIME140.dll"));
        assert!(!system_library(Pe, r"C:\untrusted\KERNEL32.dll"));
        assert!(system_library(MachO, "/usr/lib/libSystem.B.dylib"));
        assert!(!system_library(MachO, "@rpath/libcustom.dylib"));
    }
}
