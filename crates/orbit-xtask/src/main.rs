//! Repository maintenance and release verification; never linked into Orbit clients.
mod architecture;
mod bundle;
mod process;
mod remote;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(about = "Orbit repository checks and release tooling")]
struct Arguments {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Check workspace dependency and source ownership boundaries.
    Check,
    /// Build the terminal client and collect its embedded static server for verification.
    Bundle {
        #[arg(value_enum, default_value_t = Profile::Release)]
        profile: Profile,
    },
    /// Build the desktop client and its native application package.
    BundleGui {
        #[arg(value_enum, default_value_t = Profile::Release)]
        profile: Profile,
    },
    /// Archive the native desktop application with its launch assets.
    ArchiveGui {
        directory: Option<PathBuf>,
        output: Option<PathBuf>,
    },
    /// Verify standalone deployment, protocol and cache reuse in Ubuntu 20.04.
    VerifyBundle { directory: Option<PathBuf> },
    /// Verify the native client and service without a container.
    VerifyNativeBundle { directory: Option<PathBuf> },
    /// Verify native desktop window rendering and the embedded service in isolated storage.
    VerifyGuiBundle { directory: Option<PathBuf> },
    #[command(hide = true)]
    ProbeBundle { server: PathBuf },
    /// Archive a prepared bundle, preserving executable permissions.
    Archive {
        directory: Option<PathBuf>,
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Profile {
    Debug,
    Release,
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

#[tokio::main]
/// The portable-bundle test uses this Rust executable as its SSH transport stand-in.
async fn main() -> Result<()> {
    if std::env::current_exe()?
        .file_name()
        .is_some_and(|name| name == "ssh")
        || std::env::args_os()
            .next()
            .is_some_and(|name| {
                Path::new(&name)
                    .file_name()
                    .is_some_and(|name| name == "ssh")
            })
    {
        return remote::transport().await;
    }
    match Arguments::parse().command {
        Task::Check => {
            let errors = architecture::check(root())?;
            if !errors.is_empty() {
                bail!("{}", errors.join("\n"));
            }
            println!("Workspace architecture checks passed.");
        }
        Task::Bundle { profile } => bundle::build(root(), profile).await?,
        Task::BundleGui { profile } => bundle::build_gui(root(), profile).await?,
        Task::ArchiveGui { directory, output } => {
            let target = bundle::target_directory(root()).await?;
            bundle::archive_gui(
                &directory.unwrap_or_else(|| target.join("bundle-gui")),
                &output.unwrap_or_else(|| {
                    target.join(format!(
                        "orbit-desktop-{}-{}.tar.gz",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ))
                }),
            )
            .await?;
        }
        Task::VerifyBundle { directory } => {
            let directory = match directory {
                Some(path) => path,
                None => bundle::target_directory(root())
                    .await?
                    .join("bundle"),
            };
            remote::verify(&directory).await?;
        }
        Task::VerifyNativeBundle { directory } => {
            let directory = match directory {
                Some(path) => path,
                None => bundle::target_directory(root())
                    .await?
                    .join("bundle"),
            };
            remote::verify_native(&directory).await?;
        }
        Task::VerifyGuiBundle { directory } => {
            let directory = match directory {
                Some(path) => path,
                None => bundle::target_directory(root())
                    .await?
                    .join("bundle-gui"),
            };
            remote::verify_gui(&directory).await?;
        }
        Task::ProbeBundle { server } => remote::probe(&server).await?,
        Task::Archive { directory, output } => {
            let target = bundle::target_directory(root()).await?;
            bundle::archive(
                &directory.unwrap_or_else(|| target.join("bundle")),
                &output.unwrap_or_else(|| {
                    target.join(format!(
                        "orbit-{}-{}.tar.gz",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ))
                }),
            )
            .await?;
        }
    }
    Ok(())
}
