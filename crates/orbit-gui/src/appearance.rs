use anyhow::Result;
use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::{App, Entity, Window};
use gpui_shell::{HostError, HostModule, HostValue, ShellRoot, ShellRuntime, Watcher};
use std::{path::Path, rc::Rc};

pub(crate) struct Appearance {
    pub(crate) view: Entity<ShellRoot>,
    _runtime: Rc<ShellRuntime>,
    _watcher: Option<Watcher>,
}

impl Appearance {
    pub(crate) fn load(path: &Path, window: &mut Window, cx: &mut App) -> Result<Self> {
        let module = HostModule::new("orbit-appearance")
            .declarations(
                "export function isDark(): boolean;\nexport function setDark(dark: boolean): \
                    void;\n",
            )
            .function("isDark", |_| {
                gpui_shell::with_current_app(|cx| HostValue::from(cx.theme().mode.is_dark()))
                    .ok_or_else(|| HostError::new("appearance requires an active window"))
            })
            .function("setDark", |arguments| {
                let dark = arguments.boolean(0)?;
                gpui_shell::with_current_app(|cx| {
                    // Component updates its solid colors, backgrounds, Base projection,
                    // and every window together. Shell reads that same projection.
                    Theme::change(
                        if dark {
                            ThemeMode::Dark
                        } else {
                            ThemeMode::Light
                        },
                        None,
                        cx,
                    );
                    crate::preferences::Preferences::set_dark(dark, cx);
                    crate::preferences::Preferences::apply_font(cx);
                    HostValue::Null
                })
                .ok_or_else(|| HostError::new("appearance requires an active window"))
            });
        gpui_shell::policy::set_default(
            gpui_shell::policy::Policy::new().with_host_module(module)?,
        );
        let runtime = gpui_component_shell::new_isolated_runtime()?;
        let view = runtime.try_load(path, window, cx)?;
        let watcher = if cfg!(debug_assertions) {
            Some(runtime.watch(&view, window, cx)?)
        } else {
            None
        };
        Ok(Self {
            view,
            _runtime: runtime,
            _watcher: watcher,
        })
    }
}
