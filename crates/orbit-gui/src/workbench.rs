//! Window-level ownership of independent task workspaces. Changing selection
//! changes only the visible entity, never a task's transport or input state.
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
}
impl Workbench {
    pub(crate) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workbench = Self {
            runtime,
            tasks: Vec::new(),
            active: String::new(),
            unread: HashSet::new(),
        };
        workbench.open(options, window, cx);
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
        let runtime = self.runtime.clone();
        let catalog = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).options().same_host(&options))
            .map(|task| task.view.read(cx).catalog().to_vec());
        let view = cx.new(|cx| Workspace::new(options, runtime, window, cx));
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
        options.resume = None;
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
            task.view
                .update(cx, |view, cx| view.set_open_tasks(summaries.clone(), cx));
        }
        cx.notify();
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
