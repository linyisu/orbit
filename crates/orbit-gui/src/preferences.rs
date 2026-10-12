//! Application appearance preferences. The UI owns the current values; one
//! background writer serializes updates so older writes cannot win a race.
use gpui_kit::component::ThemeMode;
use gpui_kit::{App, Global, WindowAppearance};
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
}

pub(crate) struct Preferences {
    values: Values,
    changes: watch::Sender<Values>,
    _writer: tokio::task::JoinHandle<()>,
}
impl Global for Preferences {}

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
        match self.values.dark {
            Some(true) => ThemeMode::Dark,
            Some(false) => ThemeMode::Light,
            None => system.into(),
        }
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
        gpui_kit::component::Theme::update(cx, |theme| theme.font_size = gpui_kit::px(size as f32));
    }

    pub(crate) fn set_font_size(size: u8, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.values.font_size = Some(size.clamp(12, 20));
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
