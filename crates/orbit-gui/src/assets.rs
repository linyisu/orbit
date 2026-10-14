//! Component assets plus the application icons used by composer commands.
use gpui_kit::{AssetSource, SharedString};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(
    CommandIcons,
    [
        Server,
        Sparkles,
        ChartBar,
        MessageCircle,
        List,
        GitBranch,
        BookOpen,
        Puzzle,
        Bot,
        Brain,
        X,
        Wrench,
        Command,
        Monitor,
        Laptop,
        ListTodo,
        Pencil,
        Pin,
        Tags,
        Trash,
        Clock,
        Download
    ]
);
pub(crate) struct AppAssets;
impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = CommandIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(CommandIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_import_icon_is_available_in_the_application_bundle() {
        let path = gpui_kit::assets::IconName::Download.path();
        let bytes = AppAssets.load(&path).unwrap().unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("<svg")
        );
        assert!(
            AppAssets
                .list("icons/")
                .unwrap()
                .contains(&path)
        );
    }
}
