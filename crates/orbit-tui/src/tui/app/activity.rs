use orbit_client::protocol::events::AgentEvent;
use orbit_client::protocol::model::StreamEvent;

#[derive(Debug, Default)]
pub(crate) struct Activity {
    phase: Phase,
    tools: Vec<String>,
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    Waiting,
    Thinking,
    Responding,
    PreparingTools,
    Executing,
    WaitingCommands,
    WaitingTasks,
    Compacting,
}

impl Activity {
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStarted { .. } => {
                self.phase = Phase::Waiting;
            }
            AgentEvent::ContextUsage { .. } if !matches!(self.phase, Phase::Compacting) => {
                self.phase = Phase::Waiting;
            }
            AgentEvent::Model(StreamEvent::ReasoningDelta(text)) if !text.is_empty() => {
                self.phase = Phase::Thinking;
            }
            AgentEvent::Model(StreamEvent::TextDelta(text)) if !text.is_empty() => {
                self.phase = Phase::Responding;
            }
            AgentEvent::Model(StreamEvent::ToolCallDelta(_)) => {
                self.phase = Phase::PreparingTools;
            }
            AgentEvent::ToolStarted { call } => {
                self.tools.push(call.id.clone());
                self.phase = Phase::Executing;
            }
            AgentEvent::ToolFinished { call, .. }
            | AgentEvent::ToolFailed { call, .. }
            | AgentEvent::ToolBackgrounded { call, .. } => {
                self.tools.retain(|id| *id != call.id);
                if matches!(self.phase, Phase::Executing) && self.tools.is_empty() {
                    self.phase = Phase::Waiting;
                }
            }
            AgentEvent::WaitingForCommands { .. } => self.phase = Phase::WaitingCommands,
            AgentEvent::WaitingForTasks { .. } => self.phase = Phase::WaitingTasks,
            AgentEvent::CompactionStarted => self.phase = Phase::Compacting,
            AgentEvent::ContextCompacted { .. } | AgentEvent::CompactionFailed { .. } => {
                self.phase = Phase::Waiting;
            }
            AgentEvent::Completed { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled => {
                *self = Self::default();
            }
            _ => {}
        }
    }

    pub fn label(&self) -> &'static str {
        match self.phase {
            Phase::Waiting => "等待模型响应",
            Phase::Thinking => "正在思考",
            Phase::Responding => "正在回复",
            Phase::PreparingTools => "准备调用工具",
            Phase::Executing => "正在调用工具",
            Phase::WaitingCommands => "等待命令完成",
            Phase::WaitingTasks => "等待子任务完成",
            Phase::Compacting => "正在压缩上下文",
        }
    }
}
