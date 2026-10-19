use orbit_protocol::input_queue::Delivery;
use tokio::sync::oneshot;

use super::*;

pub(super) enum Draft {
    Composer(String),
    Question(Option<String>),
    UserInput(String),
    Plan,
    Model(String),
    Permissions(Mode),
    Management(String),
    Plugins { workspace: String, id: String },
}

pub(super) struct Submission {
    response: oneshot::Receiver<Result<Response>>,
    draft: Draft,
    pending_input: Option<InputReceipt>,
}

struct InputReceipt {
    ticket: u64,
    delivery: Delivery,
}

impl Submission {
    pub(super) fn starts_session(&self) -> bool {
        matches!(self.draft, Draft::Composer(_))
    }
}

impl Tab {
    fn request(&mut self, request: Request) -> Result<oneshot::Receiver<Result<Response>>> {
        if workspace::pump::independent(&request) {
            let connection = self
                .connection
                .clone()
                .ok_or_else(|| anyhow::anyhow!("工作区尚未连接"))?;
            let (reply, response) = oneshot::channel();
            self.independent_requests
                .spawn(async move {
                    // Approval and Cancel travel on the main pump while this
                    // independent handshake waits for the reviewed write result.
                    let result = async {
                        connection
                            .connect()
                            .await?
                            .request(request)
                            .await
                            .map_err(anyhow::Error::from)
                    }
                    .await;
                    let _ = reply.send(result);
                });
            return Ok(response);
        }
        let commands = self
            .commands
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("工作区尚未连接"))?;
        let (reply, response) = oneshot::channel();
        commands.try_send(Call {
            request,
            reply: Some(reply),
        })?;
        Ok(response)
    }

    pub(super) fn submit_with_receipt(&mut self, request: Request, draft: Draft) {
        let delivery = match &request {
            Request::Steer { .. } => Some(Delivery::AtBoundary),
            Request::QueueInput { .. } => Some(Delivery::NextRun),
            _ => None,
        };
        match self.request(request) {
            Ok(response) => {
                let pending_input = if let Some(delivery) = delivery
                    && let Draft::Composer(text) = &draft
                {
                    self.state.selection.clear();
                    Some(InputReceipt {
                        ticket: self
                            .state
                            .pending_input
                            .sending(text.clone()),
                        delivery,
                    })
                } else {
                    None
                };
                self.submissions.push(Submission {
                    response,
                    draft,
                    pending_input,
                });
            }
            Err(error) => self.rejected(draft, error),
        }
        self.sync_pending_permissions();
    }

    pub(super) fn ask(&mut self, question: String, follow_up: Option<usize>) {
        let question = question.trim().to_owned();
        let draft = Draft::Question(
            follow_up
                .is_none()
                .then(|| format!("/btw {question}")),
        );
        self.flush_question_close();
        if self.question_closing {
            self.rejected(
                draft,
                anyhow::anyhow!("连接繁忙，正在关闭旁路对话，请稍后重试。"),
            );
            return;
        }
        let session = self.id.clone().unwrap_or_default();
        let request = if question.is_empty() {
            Request::OpenQuestions { session }
        } else {
            Request::Ask {
                session,
                question,
                follow_up,
            }
        };
        self.submit_with_receipt(request, draft);
    }

    pub(super) fn close_questions(&mut self) {
        self.state.btw.clear();
        self.question_poll = None;
        self.question_revision = None;
        // The transport still processes queued requests in order. Drop receipts
        // here so a follow-up acknowledgement cannot reopen the closed dialog.
        self.submissions
            .retain(|s| !matches!(s.draft, Draft::Question(_)));
        self.question_closing = true;
        self.flush_question_close();
    }

    fn flush_question_close(&mut self) {
        if !self.question_closing {
            return;
        }
        let Some(commands) = &self.commands else {
            self.question_closing = false;
            return;
        };
        // Retry a full transport queue before accepting a new BTW request.
        // Never leave an invisible viewer keeping the shared request alive.
        let result = commands.try_send(Call {
            request: Request::CloseQuestions {
                session: self.id.clone().unwrap_or_default(),
            },
            reply: None,
        });
        self.question_closing = matches!(result, Err(mpsc::error::TrySendError::Full(_)));
    }

    fn rejected(&mut self, draft: Draft, error: anyhow::Error) {
        match draft {
            Draft::Plugins { workspace, id } => {
                if workspace == self.state.workspace
                    && let Some(dialog) = &mut self.state.marketplace_dialog
                    && dialog.identity() == id
                {
                    dialog.failed(error.to_string());
                }
            }
            Draft::Management(id) => {
                if let Some(panel) = self.state.management.get_mut(&id) {
                    panel.panel.error(error.to_string());
                }
            }
            Draft::Plan => self
                .state
                .planning
                .failed(error.to_string()),
            Draft::Model(_) | Draft::Permissions(_) => self
                .state
                .report_local(TranscriptItem::Error(error.to_string())),
            Draft::UserInput(id) => self
                .state
                .user_input
                .submission_error(&id, error.to_string()),
            Draft::Question(Some(text)) => self.rejected(Draft::Composer(text), error),
            Draft::Question(None) => self.state.btw.submit_error(&error),
            Draft::Composer(text) => {
                let message = if self.state.input.text.is_empty() {
                    self.state.input.set(text);
                    error.to_string()
                } else {
                    format!("{error}\n尚未确认的输入：{text}")
                };
                self.state
                    .report_local(TranscriptItem::Error(message));
            }
        }
    }

    pub(super) fn poll_submissions(&mut self) -> bool {
        while self
            .independent_requests
            .try_join_next()
            .is_some()
        {}
        let mut changed = false;
        let mut index = 0;
        while index < self.submissions.len() {
            let mut result = match self.submissions[index]
                .response
                .try_recv()
            {
                Ok(result) => result,
                Err(oneshot::error::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    Err(anyhow::anyhow!("请求确认前连接已关闭；请先重连接查看状态"))
                }
            };
            let submission = self.submissions.remove(index);
            if matches!(submission.draft, Draft::Model(_))
                && let Ok(Response::Profile(info)) = &result
            {
                self.profile(info);
                result = Ok(Response::Ok);
            }
            changed = true;
            if let Draft::Management(id) = &submission.draft {
                let id = id.clone();
                self.management_response(&id, result);
                continue;
            }
            if let Draft::Plugins { workspace, id } = &submission.draft {
                if workspace == &self.state.workspace
                    && let Some(dialog) = &mut self.state.marketplace_dialog
                    && dialog.identity() == id
                {
                    match result {
                        Ok(Response::Plugins(catalog)) => {
                            if !dialog.update_catalog(catalog) {
                                dialog.failed(
                                    "项目已变化；忽略旧插件目录，请重新打开插件市场。".into(),
                                );
                            }
                        }
                        Ok(Response::Error(error)) => dialog.failed(error),
                        Err(error) => dialog.failed(format!("{error}；请刷新确认操作结果。")),
                        _ => dialog.failed("插件响应类型不匹配，请刷新确认。".into()),
                    }
                }
                continue;
            }
            if let Some(receipt) = submission.pending_input {
                if let Ok(Response::InputQueued { message_id }) = &result {
                    self.state
                        .pending_input
                        .queued(receipt.ticket, *message_id);
                    continue;
                }
                self.state
                    .pending_input
                    .reject(receipt.ticket);
                if receipt.delivery == Delivery::NextRun && matches!(result, Ok(Response::Ok)) {
                    continue;
                }
                let error = match result {
                    Ok(Response::Error(error)) => anyhow::anyhow!(error),
                    Err(error) => error,
                    _ => anyhow::anyhow!("工作区未确认消息已入队"),
                };
                self.rejected(submission.draft, error);
                continue;
            }
            match result {
                Ok(Response::Profile(info))
                    if matches!(submission.draft, Draft::Permissions(_)) =>
                {
                    self.state
                        .permissions
                        .apply(info.mode, info.mode_revision);
                }
                Ok(Response::Ok) if !matches!(submission.draft, Draft::Permissions(_)) => {
                    if matches!(submission.draft, Draft::Plan) {
                        self.state.planning.succeeded();
                    }
                    if let Draft::UserInput(id) = &submission.draft {
                        self.state
                            .user_input
                            .submission_succeeded(id);
                        self.send(Request::UserInputHistory {
                            session: self.id.clone().unwrap_or_default(),
                            before: None,
                        });
                    }
                    if let Draft::Model(model) = submission.draft
                        && let Some(local) = self
                            .connection
                            .as_ref()
                            .and_then(|c| c.local_configuration.as_ref())
                        && let Err(error) = local.remember_model(&model)
                    {
                        self.state
                            .report_local(TranscriptItem::Error(format!(
                                "模型已切换，但未能记住选择：{error}"
                            )));
                    }
                }
                Ok(response @ Response::Questions(_)) => self.apply(response),
                Ok(Response::Error(error)) => {
                    self.rejected(submission.draft, anyhow::anyhow!(error))
                }
                Err(error) => self.rejected(submission.draft, error),
                Ok(_) => self.rejected(submission.draft, anyhow::anyhow!("工作区响应类型不匹配")),
            }
        }
        self.sync_pending_permissions();
        changed
    }

    pub(super) fn abandon_submissions(&mut self) {
        self.independent_requests.abort_all();
        let pending = std::mem::take(&mut self.submissions);
        for submission in pending {
            if let Some(receipt) = submission.pending_input {
                self.state
                    .pending_input
                    .reject(receipt.ticket);
            }
            self.rejected(
                submission.draft,
                anyhow::anyhow!("连接已断开，请重连后确认请求结果"),
            );
        }
        self.sync_pending_permissions();
    }

    fn sync_pending_permissions(&mut self) {
        self.state.permissions.pending = self
            .submissions
            .iter()
            .rev()
            .find_map(|submission| match submission.draft {
                Draft::Permissions(mode) => Some(mode),
                _ => None,
            });
    }

    pub(super) fn poll_questions(&mut self) -> bool {
        self.flush_question_close();
        let mut changed = false;
        if let Some(pending) = &mut self.question_poll {
            match pending.try_recv() {
                Ok(result) => {
                    self.question_poll = None;
                    if let Ok(response @ Response::Questions(_)) = result {
                        self.apply(response);
                        changed = true;
                    }
                }
                Err(oneshot::error::TryRecvError::Closed) => self.question_poll = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.question_poll.is_none()
            && self.state.btw.visible()
            && self.next_questions <= std::time::Instant::now()
        {
            if let Some(session) = &self.id {
                self.question_poll = self
                    .request(Request::ReadQuestions {
                        session: session.clone(),
                        after: self.question_revision,
                    })
                    .ok();
            }
            self.next_questions = std::time::Instant::now() + Duration::from_millis(250);
        }
        changed
    }
}
