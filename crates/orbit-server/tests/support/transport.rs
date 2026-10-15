//! Native transport stand-in for shared-client SSH/WSL integration tests.
use std::{
    io::{self, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::Command,
};

fn main() {
    let arguments: Vec<_> = std::env::args_os().collect();
    println!("test login banner");
    io::stdout().flush().unwrap();
    let mut command = if Path::new(&arguments[0]).file_name().unwrap() == "ssh" {
        assert_eq!(arguments[arguments.len() - 2], "test-alias");
        let mut command = Command::new("sh");
        command.arg("-c").arg(arguments.last().unwrap());
        command
    } else {
        assert_eq!(arguments[1], "--distribution");
        assert_eq!(arguments[2], "Test Distro");
        let start = arguments
            .iter()
            .position(|argument| argument == "--exec")
            .unwrap()
            + 1;
        let mut command = Command::new(&arguments[start]);
        command.args(&arguments[start + 1..]);
        command
    };
    panic!("cannot launch test transport: {}", command.exec());
}
