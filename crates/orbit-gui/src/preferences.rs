//! Application appearance preferences. The UI owns the current values; one
//! background writer serializes updates so older writes cannot win a race.
use gpui_kit::component::ThemeMode;
use gpui_kit::{App, Global, SharedString, Subscription, Window, WindowAppearance};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::watch;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Values {
    dark: Option<bool>,
    sidebar_rem: Option<f32>,
    comfortable_width: bool,
    font_size: Option<u8>,
    ui_font_family: Option<String>,
    code_font_family: Option<String>,
}

pub(crate) struct Preferences {
    values: Values,
    changes: watch::Sender<Values>,
    _writer: tokio::task::JoinHandle<()>,
}
impl Global for Preferences {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorMode {
    System,
    Light,
    Dark,
}

impl Values {
    fn color_mode(&self) -> ColorMode {
        match self.dark {
            None => ColorMode::System,
            Some(false) => ColorMode::Light,
            Some(true) => ColorMode::Dark,
        }
    }
}

impl Preferences {
    pub(crate) fn load(
        path: Option<&Path>,
        runtime: &tokio::runtime::Runtime,
    ) -> anyhow::Result<Self> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
                .ok_or_else(|| anyhow::anyhow!("找不到设置目录，请指定 --preferences"))?
                .join("orbit/gui.json"),
        };
        // Read once before the UI event loop starts, never from render.
        let values = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Values::default(),
            Err(error) => return Err(error.into()),
        };
        let (changes, mut receiver) = watch::channel(values.clone());
        let writer = runtime.spawn(async move {
            while receiver.changed().await.is_ok() {
                let values = receiver.borrow_and_update().clone();
                let path = path.clone();
                let result = tokio::task::spawn_blocking(move || save(&path, &values)).await;
                match result {
                    Ok(Ok(())) => {}
                    error => eprintln!("无法保存 GUI 设置：{error:?}"),
                }
            }
        });
        Ok(Self {
            values,
            changes,
            _writer: writer,
        })
    }

    pub(crate) fn mode(&self, system: WindowAppearance) -> ThemeMode {
        match self.values.color_mode() {
            ColorMode::System => system.into(),
            ColorMode::Light => ThemeMode::Light,
            ColorMode::Dark => ThemeMode::Dark,
        }
    }

    pub(crate) fn color_mode(cx: &App) -> ColorMode {
        cx.global::<Self>().values.color_mode()
    }

    pub(crate) fn set_color_mode(mode: ColorMode, window: &mut Window, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        // The existing nullable value already represents all three choices,
        // so old preference files retain their exact meaning.
        this.values.dark = match mode {
            ColorMode::System => None,
            ColorMode::Light => Some(false),
            ColorMode::Dark => Some(true),
        };
        this.changes.send_replace(this.values.clone());
        let resolved = cx.global::<Self>().mode(window.appearance());
        gpui_kit::component::Theme::change(resolved, Some(window), cx);
        Self::apply_font(cx);
    }

    pub(crate) fn watch_appearance(window: &Window, _: &mut App) -> Subscription {
        window.observe_window_appearance(|window, cx| {
            if Self::color_mode(cx) == ColorMode::System {
                let mode = cx.global::<Self>().mode(window.appearance());
                gpui_kit::component::Theme::change(mode, Some(window), cx);
                Self::apply_font(cx);
            }
        })
    }

    pub(crate) fn sidebar(cx: &App) -> f32 {
        cx.global::<Self>()
            .values
            .sidebar_rem
            .filter(|width| width.is_finite())
            .unwrap_or(16.)
            .clamp(12., 24.)
    }

    pub(crate) fn set_sidebar(width: f32, cx: &mut App) {
        if !width.is_finite() {
            return;
        }
        let this = cx.global_mut::<Self>();
        this.values.sidebar_rem = Some(width.clamp(12., 24.));
        this.changes.send_replace(this.values.clone());
    }

    pub(crate) fn set_dark(dark: bool, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.dark = Some(dark);
        this.changes.send_replace(this.values.clone());
    }

    pub(crate) fn comfortable_width(cx: &App) -> bool {
        cx.global::<Self>().values.comfortable_width
    }

    pub(crate) fn set_comfortable_width(value: bool, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.comfortable_width = value;
        this.changes.send_replace(this.values.clone());
        cx.refresh_windows();
    }

    pub(crate) fn font_size(cx: &App) -> u8 {
        cx.global::<Self>()
            .values
            .font_size
            .unwrap_or(16)
            .clamp(12, 20)
    }

    pub(crate) fn apply_font(cx: &mut App) {
        let size = Self::font_size(cx);
        let values = &cx.global::<Self>().values;
        let (ui, code) = crate::theme::resolve_fonts(
            values.ui_font_family.as_deref(),
            values.code_font_family.as_deref(),
            cx,
        );
        gpui_kit::component::Theme::update(cx, |theme| {
            theme.font_size = gpui_kit::px(size as f32);
            theme.font_family = ui;
            theme.mono_font_family = code;
        });
    }

    pub(crate) fn set_font_size(size: u8, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.font_size = Some(size.clamp(12, 20));
        this.changes.send_replace(this.values.clone());
        Self::apply_font(cx);
    }

    pub(crate) fn ui_font(cx: &App) -> Option<SharedString> {
        cx.global::<Self>()
            .values
            .ui_font_family
            .as_ref()
            .map(|family| family.clone().into())
    }

    pub(crate) fn code_font(cx: &App) -> Option<SharedString> {
        cx.global::<Self>()
            .values
            .code_font_family
            .as_ref()
            .map(|family| family.clone().into())
    }

    pub(crate) fn set_ui_font(family: Option<SharedString>, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.ui_font_family = family.map(|family| family.to_string());
        this.changes.send_replace(this.values.clone());
        Self::apply_font(cx);
    }

    pub(crate) fn set_code_font(family: Option<SharedString>, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.code_font_family = family.map(|family| family.to_string());
        this.changes.send_replace(this.values.clone());
        Self::apply_font(cx);
    }
}

fn save(path: &Path, values: &Values) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(values)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_preferences_keep_default_fonts_and_font_choices_roundtrip() {
        let mut values: Values = serde_json::from_str(r#"{"font_size":18,"dark":true}"#).unwrap();
        assert!(values.ui_font_family.is_none());
        assert!(values.code_font_family.is_none());
        assert_eq!(values.font_size, Some(18));
        values.ui_font_family = Some("Noto Sans CJK SC".into());
        values.code_font_family = Some("JetBrains Mono".into());
        let restored: Values =
            serde_json::from_slice(&serde_json::to_vec(&values).unwrap()).unwrap();
        assert_eq!(restored.ui_font_family, values.ui_font_family);
        assert_eq!(restored.code_font_family, values.code_font_family);
        assert_eq!(restored.font_size, values.font_size);
    }

    #[test]
    fn color_modes_preserve_existing_preferences_and_system_choice() {
        for (json, expected) in [
            (r#"{}"#, ColorMode::System),
            (r#"{"dark":null}"#, ColorMode::System),
            (r#"{"dark":false}"#, ColorMode::Light),
            (r#"{"dark":true}"#, ColorMode::Dark),
        ] {
            let values: Values = serde_json::from_str(json).unwrap();
            assert_eq!(values.color_mode(), expected);
            let restored: Values =
                serde_json::from_slice(&serde_json::to_vec(&values).unwrap()).unwrap();
            assert_eq!(restored.color_mode(), expected);
        }
    }
}
