//! Application appearance preferences. The UI owns the current values; one
//! background writer serializes updates so older writes cannot win a race.
use gpui_kit::component::ThemeMode;
use gpui_kit::{App, Global, SharedString, Subscription, Task, Window, WindowAppearance};
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
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    keybindings: crate::keymap::Overrides,
}

#[derive(Clone)]
struct SaveRequest {
    revision: u64,
    values: Values,
}

pub(crate) struct Preferences {
    values: Values,
    revision: u64,
    save_status: crate::keymap::SaveStatus,
    changes: watch::Sender<SaveRequest>,
    save_updates: watch::Receiver<crate::keymap::SaveStatus>,
    _save_observer: Option<Task<()>>,
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
        let values: Values = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Values::default(),
            Err(error) => return Err(error.into()),
        };
        let (changes, mut receiver) = watch::channel(SaveRequest {
            revision: 0,
            values: values.clone(),
        });
        let save_status = crate::keymap::SaveStatus::Saved(0);
        let (saved, save_updates) = watch::channel(save_status.clone());
        let writer = runtime.spawn(async move {
            while receiver.changed().await.is_ok() {
                let request = receiver.borrow_and_update().clone();
                let revision = request.revision;
                let path = path.clone();
                let result =
                    tokio::task::spawn_blocking(move || save(&path, &request.values)).await;
                let status = match result {
                    Ok(Ok(())) => crate::keymap::SaveStatus::Saved(revision),
                    result => {
                        let message = match result {
                            Ok(Err(error)) => format!("{error:#}"),
                            Err(error) => error.to_string(),
                            Ok(Ok(())) => unreachable!(),
                        };
                        eprintln!("无法保存 GUI 设置：{message}");
                        crate::keymap::SaveStatus::Failed { revision, message }
                    }
                };
                saved.send_replace(status);
            }
        });
        Ok(Self {
            values,
            revision: 0,
            save_status,
            changes,
            save_updates,
            _save_observer: None,
            _writer: writer,
        })
    }

    /// The foreground observer delivers disk receipts to every settings window.
    /// The writer can coalesce values, but an older receipt cannot acknowledge a
    /// newer edit or hide its error.
    pub(crate) fn observe_saves(cx: &mut App) {
        if cx.global::<Self>()._save_observer.is_some() {
            return;
        }
        let mut updates = cx.global::<Self>().save_updates.clone();
        let observer = cx.spawn(async move |cx| {
            while updates.changed().await.is_ok() {
                let status = updates.borrow_and_update().clone();
                cx.update(|cx| {
                    let this = cx.global_mut::<Self>();
                    if accepts_receipt(this.revision, &status) {
                        this.save_status = status;
                    }
                });
            }
        });
        cx.global_mut::<Self>()._save_observer = Some(observer);
    }

    fn publish(&mut self) -> u64 {
        self.revision += 1;
        self.save_status = crate::keymap::SaveStatus::Saving(self.revision);
        self.changes.send_replace(SaveRequest {
            revision: self.revision,
            values: self.values.clone(),
        });
        if self.changes.is_closed() {
            self.save_status = crate::keymap::SaveStatus::Failed {
                revision: self.revision,
                message: "设置保存任务已停止，请重新启动应用后重试".into(),
            };
        }
        self.revision
    }

    pub(crate) fn keybindings(cx: &App) -> &crate::keymap::Overrides {
        &cx.global::<Self>().values.keybindings
    }

    pub(crate) fn set_keybindings(values: crate::keymap::Overrides, cx: &mut App) -> u64 {
        let this = cx.global_mut::<Self>();
        this.values.keybindings = values;
        this.publish()
    }

    pub(crate) fn save_status(cx: &App) -> crate::keymap::SaveStatus {
        cx.global::<Self>().save_status.clone()
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
        this.publish();
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
        this.publish();
    }

    pub(crate) fn set_dark(dark: bool, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.dark = Some(dark);
        this.publish();
    }

    pub(crate) fn comfortable_width(cx: &App) -> bool {
        cx.global::<Self>().values.comfortable_width
    }

    pub(crate) fn set_comfortable_width(value: bool, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.comfortable_width = value;
        this.publish();
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
        this.publish();
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
        this.publish();
        Self::apply_font(cx);
    }

    pub(crate) fn set_code_font(family: Option<SharedString>, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.code_font_family = family.map(|family| family.to_string());
        this.publish();
        Self::apply_font(cx);
    }
}

fn accepts_receipt(current_revision: u64, status: &crate::keymap::SaveStatus) -> bool {
    current_revision == status.revision()
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

    #[test]
    fn shortcut_overrides_roundtrip_and_old_preferences_use_defaults() {
        let mut values: Values = serde_json::from_str(r#"{"font_size":18}"#).unwrap();
        assert!(values.keybindings.is_empty());
        values
            .keybindings
            .insert("toggle_sidebar".into(), Some("ctrl-k ctrl-b".into()));
        values.keybindings.insert("open_project".into(), None);
        values
            .keybindings
            .insert("future_command".into(), Some("ctrl-f12".into()));
        let restored: Values =
            serde_json::from_slice(&serde_json::to_vec(&values).unwrap()).unwrap();
        assert_eq!(restored.keybindings, values.keybindings);
        assert_eq!(restored.font_size, Some(18));
    }

    #[test]
    fn stale_save_receipts_cannot_acknowledge_a_later_edit() {
        use crate::keymap::SaveStatus;
        assert!(!accepts_receipt(2, &SaveStatus::Saved(1)));
        assert!(!accepts_receipt(
            2,
            &SaveStatus::Failed {
                revision: 1,
                message: "old error".into()
            }
        ));
        assert!(accepts_receipt(2, &SaveStatus::Saved(2)));
        assert!(accepts_receipt(
            2,
            &SaveStatus::Failed {
                revision: 2,
                message: "disk full".into()
            }
        ));
    }

    #[test]
    fn failed_write_can_retry_the_same_values_with_a_new_receipt() {
        let (changes, mut receiver) = watch::channel(SaveRequest {
            revision: 0,
            values: Values::default(),
        });
        let (_, save_updates) = watch::channel(crate::keymap::SaveStatus::Saved(0));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut preferences = Preferences {
            values: Values::default(),
            revision: 0,
            save_status: crate::keymap::SaveStatus::Saved(0),
            changes,
            save_updates,
            _save_observer: None,
            _writer: runtime.spawn(async {}),
        };
        assert_eq!(preferences.publish(), 1);
        preferences.save_status = crate::keymap::SaveStatus::Failed {
            revision: 1,
            message: "disk full".into(),
        };
        receiver.borrow_and_update();
        assert_eq!(preferences.publish(), 2);
        assert!(receiver.has_changed().unwrap());
        assert_eq!(receiver.borrow().revision, 2);
        assert_eq!(
            preferences.save_status,
            crate::keymap::SaveStatus::Saving(2)
        );
    }

    #[test]
    fn disk_failure_preserves_values_and_same_value_retry_is_persisted() {
        use crate::keymap::SaveStatus;
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("settings");
        let path = parent.join("gui.json");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut preferences = Preferences::load(Some(&path), &runtime).unwrap();
        std::fs::write(&parent, "blocks directory creation").unwrap();
        preferences
            .values
            .keybindings
            .insert("toggle_sidebar".into(), Some("ctrl-k ctrl-b".into()));
        let first = preferences.publish();
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                preferences.save_updates.changed(),
            )
            .await
            .unwrap()
            .unwrap();
        });
        assert!(
            matches!(&*preferences.save_updates.borrow_and_update(), SaveStatus::Failed { revision, .. } if *revision == first)
        );
        assert_eq!(
            preferences.values.keybindings["toggle_sidebar"].as_deref(),
            Some("ctrl-k ctrl-b")
        );
        std::fs::remove_file(&parent).unwrap();
        let second = preferences.publish();
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                preferences.save_updates.changed(),
            )
            .await
            .unwrap()
            .unwrap();
        });
        assert_eq!(
            *preferences.save_updates.borrow(),
            SaveStatus::Saved(second)
        );
        let restored: Values = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(restored.keybindings, preferences.values.keybindings);
    }
}
