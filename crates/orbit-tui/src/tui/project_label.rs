use std::path::Path;

pub(super) fn path_label(workspace: &str, home: Option<&Path>) -> String {
    let path = Path::new(workspace);
    if let Some(relative) = home.and_then(|home| path.strip_prefix(home).ok()) {
        if relative.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", relative.display());
    }

    workspace.to_owned()
}

pub(super) use crate::workspace::project::branch;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_abbreviation_respects_path_component_boundaries() {
        let home = Path::new("/home/test");
        assert_eq!(path_label("/home/test/project", Some(home)), "~/project");
        assert_eq!(path_label("/home/test", Some(home)), "~");
        assert_eq!(
            path_label("/home/test-other/project", Some(home)),
            "/home/test-other/project"
        );
        assert_eq!(path_label("/tmp/project", None), "/tmp/project");
    }
}
