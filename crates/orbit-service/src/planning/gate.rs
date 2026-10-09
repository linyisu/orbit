use orbit_agent::{
    background::{BackgroundJobs, JobStatus},
    tool::{ToolCapabilities, ToolEffect, ToolError, ToolPolicy},
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
    fn visible(&self, capabilities: ToolCapabilities) -> bool {
        !self.access.lock().unwrap().read_only || capabilities.in_plan()
    }
    fn acquire(
        &self,
        call: &ToolCall,
        capabilities: ToolCapabilities,
    ) -> Result<Box<dyn Send>, ToolError> {
        let mut access = self.access.lock().unwrap();
        let name = call.function.name.as_str();
        // Planning controls acquire their own transition guard; counting the
        // enter_plan call itself would prevent the transition from ever being idle.
        if capabilities.effect == ToolEffect::Planning {
            return Ok(Box::new(()));
        }
        if access.read_only && !capabilities.in_plan() {
            return Err(ToolError::ExecutionFailed(format!(
                "Plan 为只读模式，禁止 {name}；请提交计划并等待用户明确选择开始执行"
            )));
        }
        access.active += 1;
        Ok(Box::new(Lease(self.access.clone())))
    }
}
