use super::*;
use crate::tui::{
    dialogs::{InfoDialog, ReportSource},
    usage,
};

impl Tab {
    pub(super) fn show_usage(&mut self) {
        let mut dialog = InfoDialog::new("用量", String::new());
        dialog.source = Some(ReportSource::Usage);
        self.state.usage.update_dialog(&mut dialog);
        self.state.info_dialog = Some(dialog);
        if !self.state.usage.fresh() {
            self.refresh_usage();
        }
    }

    fn refresh_usage(&mut self) {
        if self.usage_task.is_some() {
            return;
        }
        let Some((connection, session)) = self.connection.clone().zip(self.id.clone()) else {
            self.state.usage.error = Some("请先连接工作区".into());
            self.update_usage_dialog();
            return;
        };
        self.state.usage.loading = true;
        self.state.usage.error = None;
        self.update_usage_dialog();
        // A slow quota endpoint must not occupy the conversation's transport
        // queue. The separate connection attaches to the same running service.
        self.usage_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            let mut client = connection.connect().await?;
            match client.request(Request::Usage { session }).await? {
                Response::Usage(snapshot) => Ok(snapshot),
                _ => anyhow::bail!("工作区服务未返回用量信息"),
            }
        })));
    }

    fn update_usage_dialog(&mut self) {
        if usage::is_open(&self.state.info_dialog)
            && let Some(dialog) = &mut self.state.info_dialog
        {
            self.state.usage.update_dialog(dialog);
        }
    }

    pub(super) fn invalidate_usage(&mut self) {
        self.usage_task = None;
        self.state.usage = Default::default();
        if usage::is_open(&self.state.info_dialog) {
            self.state.info_dialog = None;
        }
    }

    pub(super) async fn poll_usage(&mut self) -> bool {
        if !usage::is_open(&self.state.info_dialog) {
            self.usage_task = None;
            self.state.usage.loading = false;
            return false;
        }
        if self
            .state
            .info_dialog
            .as_mut()
            .is_some_and(|dialog| std::mem::take(&mut dialog.refresh_requested))
        {
            self.refresh_usage();
            return true;
        }
        if !self
            .usage_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            return false;
        }
        match self.usage_task.take().unwrap().await {
            Ok(Ok(snapshot)) => self.state.usage.receive(snapshot),
            _ => {
                self.state.usage.loading = false;
                self.state.usage.error = Some("无法查询用量，请检查连接后按 R 重试".into());
            }
        }
        self.update_usage_dialog();
        true
    }
}
