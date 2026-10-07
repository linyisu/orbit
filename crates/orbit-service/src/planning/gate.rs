use orbit_agent::{
    background::{BackgroundJobs, JobStatus},
    tool::{ToolError, ToolPolicy},
};
use orbit_model::types::ToolCall;
use std::sync::{Arc, Mutex, RwLock};

#[derive(Default)]
struct Access {
    read_only: bool,
    active: usize,
}
#[derive(Clone, Default)]
pub(super) struct Gate {
    access: Arc<Mutex<Access>>,
    jobs: Arc<RwLock<Vec<BackgroundJobs>>>,
    context: Arc<RwLock<Option<String>>>,
}
fn safe(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_files"
            | "search_text"
            | "project_overview"
            | "read_document"
            | "read_image"
            | "web_search"
            | "web_fetch"
            | "git_inspect"
            | "read_history"
            | "load_skill"
            | "task_list"
            | "memory_list"
            | "memory_search"
            | "ask_user"
            | "read_agent"
            | "wait_agents"
            | "cancel_agents"
            | "enter_plan"
            | "update_plan"
            | "submit_plan"
    )
}
impl Gate {
    pub fn context(&self, text: String) {
        *self.context.write().unwrap() = Some(text);
    }
    pub fn attach(&self, jobs: Vec<BackgroundJobs>) {
        *self.jobs.write().unwrap() = jobs;
    }
    pub fn idle(&self) -> bool {
        self.jobs.read().unwrap().iter().all(|jobs| {
            jobs.snapshots(&[], false).is_ok_and(|jobs| {
                jobs.iter()
                    .all(|j| !matches!(j.status, JobStatus::Running | JobStatus::Cancelling))
            })
        })
    }
    pub fn enter(&self) -> anyhow::Result<()> {
        let mut access = self.access.lock().unwrap();
        anyhow::ensure!(
            access.active == 0 && self.idle(),
            "请等待正在执行的工具及后台任务结束，再进入 Plan"
        );
        access.read_only = true;
        Ok(())
    }
    pub fn set(&self, read_only: bool) {
        self.access.lock().unwrap().read_only = read_only;
    }
}
struct Lease(Arc<Mutex<Access>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.lock().unwrap().active -= 1;
    }
}
impl ToolPolicy for Gate {
    fn context(&self) -> Option<String> {
        self.context.read().unwrap().clone()
    }
    fn visible(&self, name: &str) -> bool {
        !self.access.lock().unwrap().read_only
            || safe(name)
            || matches!(name, "spawn_agent" | "send_agent_message")
    }
    fn acquire(&self, call: &ToolCall) -> Result<Box<dyn Send>, ToolError> {
        let mut access = self.access.lock().unwrap();
        let name = call.function.name.as_str();
        if safe(name) {
            return Ok(Box::new(()));
        }
        let explore = name == "spawn_agent"
            && call.arguments_json().is_ok_and(|v| {
                matches!(
                    v.get("role").and_then(|r| r.as_str()).unwrap_or("explore"),
                    "explore" | "review"
                )
            });
        if access.read_only && !explore && name != "send_agent_message" {
            return Err(ToolError::ExecutionFailed(format!(
                "Plan 为只读模式，禁止 {name}；请提交计划并等待用户明确选择开始执行"
            )));
        }
        access.active += 1;
        Ok(Box::new(Lease(self.access.clone())))
    }
}
