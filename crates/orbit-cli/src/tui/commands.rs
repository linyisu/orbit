#[derive(Clone, Copy)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub needs_argument: bool,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/new",
        description: "Start a new session; keep the current one saved",
        needs_argument: false,
    },
    Command {
        name: "/resume",
        description: "Choose or open a saved session",
        needs_argument: false,
    },
    Command {
        name: "/sessions",
        description: "Browse sessions in this workspace",
        needs_argument: false,
    },
    Command {
        name: "/rename",
        description: "Give the current session a title",
        needs_argument: true,
    },
    Command {
        name: "/changes",
        description: "List files changed by Orbit in this session",
        needs_argument: false,
    },
    Command {
        name: "/diff",
        description: "Show the last recorded change for a file",
        needs_argument: true,
    },
    Command {
        name: "/undo",
        description: "Restore the latest Orbit file change, optionally by path",
        needs_argument: false,
    },
    Command {
        name: "/model",
        description: "Choose the model for the next response",
        needs_argument: false,
    },
    Command {
        name: "/reasoning",
        description: "Show or select a reasoning level",
        needs_argument: false,
    },
    Command {
        name: "/attach",
        description: "Attach an image from the workspace",
        needs_argument: true,
    },
    Command {
        name: "/detach",
        description: "Clear pending image attachments",
        needs_argument: false,
    },
    Command {
        name: "/status",
        description: "Show session and connection status",
        needs_argument: false,
    },
    Command {
        name: "/context",
        description: "Show the active context budget",
        needs_argument: false,
    },
    Command {
        name: "/compact",
        description: "Archive older context and retain recent work",
        needs_argument: false,
    },
    Command {
        name: "/mode",
        description: "Show or change approval mode",
        needs_argument: false,
    },
    Command {
        name: "/help",
        description: "Show input and navigation shortcuts",
        needs_argument: false,
    },
    Command {
        name: "/exit",
        description: "Exit Orbit",
        needs_argument: false,
    },
];

pub fn matching(input: &str) -> Vec<&'static Command> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return Vec::new();
    }

    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(input))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_only_offers_implemented_commands_before_arguments() {
        assert_eq!(matching("/res")[0].name, "/resume");
        assert!(matching("/resume old").is_empty());
        assert!(matching("ordinary prompt").is_empty());
        assert!(matching("/skills").is_empty());
        assert!(matching("/mcp").is_empty());
    }
}
