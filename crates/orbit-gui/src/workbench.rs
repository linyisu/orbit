//! Window-level ownership of independent task workspaces. Changing selection
//! changes only the visible entity, never a task's transport or input state.
pub(crate) mod storage;
use crate::{
    backend::Options,
    workspace::{Workspace, WorkspaceEvent},
};
use gpui_kit::*;
use std::{collections::HashSet, sync::Arc};

struct OpenTask {
    view: Entity<Workspace>,
    _subscription: Subscription,
    was_busy: bool,
}
pub(crate) struct Workbench {
    runtime: Arc<tokio::runtime::Runtime>,
    tasks: Vec<OpenTask>,
    active: String,
    unread: HashSet<String>,
    origin: Options,
    storage: storage::Storage,
    restoring: bool,
    _storage_errors: Task<()>,
}
impl Workbench {
    pub(crate) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        loaded: storage::Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let storage::Loaded {
            storage,
            mut saved,
            mut errors,
        } = loaded;
        if let Some(snapshot) = &mut saved {
            snapshot.deduplicate_tabs();
        }
        let mut workbench = Self {
            runtime,
            tasks: Vec::new(),
            active: String::new(),
            unread: HashSet::new(),
            origin: options.clone(),
            storage,
            restoring: true,
            _storage_errors: cx.spawn(async move |owner, cx| {
                while errors.changed().await.is_ok() {
                    let error = errors.borrow_and_update().clone();
                    if let Some(error) = error {
                        let _ = owner.update_in(cx, |_, window, cx| {
                            use gpui_kit::component::{WindowExt, notification::Notification};
                            window.push_notification(
                                Notification::error(error).title("草稿尚未保存"),
                                cx,
                            );
                        });
                    }
                }
            }),
        };
        let active = saved.as_ref().map(|saved| saved.active.clone());
        if let Some(saved) = saved {
            for task in saved.tasks {
                workbench.create(task.options.clone(), Some(task), window, cx);
            }
        }
        if workbench.tasks.is_empty()
            || options.workspace.is_some()
            || options.resume.is_some()
            || options.ssh.is_some()
            || options.wsl.is_some()
        {
            workbench.open(options, window, cx);
        } else if let Some(active) = active {
            workbench.activate(&active, window, cx);
        }
        workbench.restoring = false;
        // Component overlays are installed after the root view constructor.
        // Activate only once, after that installation, instead of exposing each
        // restored tab and attempting to dismiss its notifications in bootstrap.
        cx.defer_in(window, |workbench, window, cx| {
            let active = workbench.active.clone();
            workbench.activate(&active, window, cx);
        });
        workbench
    }
    fn open(&mut self, options: Options, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .tasks
            .iter()
            .find(|task| {
                let view = task.view.read(cx);
                if !view.options().same_host(&options) {
                    return false;
                }
                match options.resume.as_deref() {
                    Some(id) => view.can_resume(id, &options),
                    None => {
                        view.options().workspace == options.workspace && view.is_empty_draft(cx)
                    }
                }
            })
            .map(|task| task.view.read(cx).key().to_owned());
        if let Some(key) = existing {
            self.activate(&key, window, cx);
            return;
        }
        self.create(options, None, window, cx);
    }
    fn create(
        &mut self,
        options: Options,
        saved: Option<storage::SavedTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let runtime = self.runtime.clone();
        let catalog = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).options().same_host(&options))
            .map(|task| task.view.read(cx).catalog().to_vec());
        let view = cx.new(|cx| Workspace::new(options, runtime, window, cx));
        if let Some(saved) = saved {
            view.update(cx, |view, cx| view.restore(saved, window, cx));
        }
        if let Some(catalog) = catalog {
            view.update(cx, |view, cx| view.set_catalog(catalog, cx));
        }
        let subscription = cx.subscribe_in(
            &view,
            window,
            |workbench, source, event, window, cx| match event {
                WorkspaceEvent::Open(options) => workbench.open((**options).clone(), window, cx),
                WorkspaceEvent::Activate(key) => workbench.activate(key, window, cx),
                WorkspaceEvent::Close(key) => workbench.close(key, window, cx),
                WorkspaceEvent::DraftChanged => workbench.persist(cx),
                WorkspaceEvent::Changed => {
                    let summary = source.read(cx).summary();
                    let finished = workbench
                        .tasks
                        .iter_mut()
                        .find(|task| task.view == *source)
                        .is_some_and(|task| {
                            let finished = task.was_busy && !summary.busy;
                            task.was_busy = summary.busy;
                            finished
                        });
                    if summary.key != workbench.active && (summary.attention || finished) {
                        workbench.unread.insert(summary.key);
                    }
                    workbench.publish(cx);
                }
                WorkspaceEvent::Catalog(catalog) => {
                    let options = source.read(cx).options().clone();
                    for task in &workbench.tasks {
                        if task.view != *source && task.view.read(cx).options().same_host(&options)
                        {
                            task.view
                                .update(cx, |view, cx| view.set_catalog(catalog.clone(), cx));
                        }
                    }
                }
            },
        );
        let key = view.read(cx).key().to_owned();
        self.tasks.push(OpenTask {
            view,
            _subscription: subscription,
            was_busy: false,
        });
        self.activate(&key, window, cx);
    }
    fn activate(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .tasks
            .iter()
            .any(|task| task.view.read(cx).key() == key)
        {
            return;
        }
        self.active = key.to_owned();
        if self.restoring {
            return;
        }
        self.unread.remove(key);
        for task in &self.tasks {
            let visible = task.view.read(cx).key() == key;
            task.view
                .update(cx, |view, cx| view.set_visible(visible, window, cx));
        }
        self.publish(cx);
    }
    fn close(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self
            .tasks
            .iter()
            .position(|task| task.view.read(cx).key() == key)
        else {
            return;
        };
        // Keep active transports alive; stopping a run is an explicit action.
        if self.tasks[ix].view.read(cx).summary().busy {
            return;
        }
        let mut options = self.tasks[ix].view.read(cx).options().clone();
        // A failed restored tab may still carry a removed model in its launch
        // options. Closing it must leave a usable blank task with the default.
        options.model = None;
        options.resume = None;
        options.recover_draft = false;
        options.relink_on_resume = false;
        let was_active = self.active == key;
        self.tasks.remove(ix);
        self.unread.remove(key);
        if self.tasks.is_empty() {
            self.open(options, window, cx);
        } else if was_active {
            let next = self.tasks[ix.min(self.tasks.len() - 1)]
                .view
                .read(cx)
                .key()
                .to_owned();
            self.activate(&next, window, cx);
        } else {
            self.publish(cx);
        }
    }
    fn publish(&self, cx: &mut Context<Self>) {
        let summaries: Vec<_> = self
            .tasks
            .iter()
            .map(|task| {
                let mut summary = task.view.read(cx).summary();
                summary.unread = self.unread.contains(&summary.key);
                summary
            })
            .collect();
        for task in &self.tasks {
            // Session IDs are scoped to a service/data source, just like the catalog.
            let options = task.view.read(cx).options();
            let navigation_tasks = self
                .tasks
                .iter()
                .zip(&summaries)
                .filter(|(other, _)| other.view.read(cx).options().same_host(options))
                .map(|(_, summary)| summary.clone())
                .collect();
            task.view.update(cx, |view, cx| {
                view.set_open_tasks(summaries.clone(), navigation_tasks, cx)
            });
        }
        cx.notify();
        self.persist(cx);
    }
    fn persist(&self, cx: &App) {
        if self.restoring {
            return;
        }
        self.storage.save(storage::Snapshot::new(
            self.origin.clone(),
            self.active.clone(),
            self.tasks
                .iter()
                .map(|task| task.view.read(cx).saved(cx))
                .collect(),
        ));
    }
}
impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(
            self.tasks
                .iter()
                .find(|task| task.view.read(cx).key() == self.active)
                .map(|task| task.view.clone()),
        )
    }
}
