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
        Cloud,
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

/// Verify deployment assets without accepting RustEmbed's development filesystem fallback.
pub(crate) fn embedded_icon_count() -> anyhow::Result<usize> {
    let paths = AppAssets.list("icons/")?;
    anyhow::ensure!(!paths.is_empty(), "图标资源列表为空");
    for path in &paths {
        let bytes = AppAssets
            .load(path)?
            .ok_or_else(|| anyhow::anyhow!("缺少图标资源：{path}"))?;
        anyhow::ensure!(matches!(bytes, Cow::Borrowed(_)), "图标未内嵌：{path}");
        anyhow::ensure!(
            std::str::from_utf8(&bytes)?.contains("<svg"),
            "图标无效：{path}"
        );
    }
    Ok(paths.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_caption_icons_are_embedded_in_debug_deployments() {
        assert!(embedded_icon_count().unwrap() > 20);
        for icon in [
            gpui_kit::assets::IconName::Plus,
            gpui_kit::assets::IconName::Search,
            gpui_kit::assets::IconName::Settings,
            gpui_kit::assets::IconName::PanelLeft,
            gpui_kit::assets::IconName::PanelRight,
            gpui_kit::assets::IconName::WindowMinimize,
            gpui_kit::assets::IconName::WindowMaximize,
            gpui_kit::assets::IconName::WindowRestore,
            gpui_kit::assets::IconName::WindowClose,
        ] {
            assert!(matches!(
                AppAssets.load(&icon.path()).unwrap(),
                Some(Cow::Borrowed(_))
            ));
        }
    }

    #[test]
    fn source_import_and_environment_icons_are_available_in_the_application_bundle() {
        for icon in [
            gpui_kit::assets::IconName::Download,
            gpui_kit::assets::IconName::Cloud,
        ] {
            let path = icon.path();
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
}
