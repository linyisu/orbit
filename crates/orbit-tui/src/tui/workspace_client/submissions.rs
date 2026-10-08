use tokio::sync::oneshot;

use super::*;

pub(super) enum Draft {
    Composer(String),
    Question,
    UserInput(String),
    Plan,
    Model(String),
    Permissions(Mode),
}

pub(super) struct Submission {
    response: oneshot::Receiver<Result<Response>>,
    draft: Draft,
}

impl Submission {
    pub(super) fn starts_session(&self) -> bool {
        matches!(self.draft, Draft::Composer(_) | Draft::Question)
    }
}

impl Tab {
    fn request(&self, request: Request) -> Result<oneshot::Receiver<Result<Response>>> {
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
        match self.request(request) {
            Ok(response) => self.submissions.push(Submission { response, draft }),
            Err(error) => self.rejected(draft, error),
        }
        self.sync_pending_permissions();
    }

    pub(super) fn ask(&mut self, question: String, follow_up: Option<usize>) {
        let question = question.trim().to_owned();
        let draft = if follow_up.is_some() {
            Draft::Question
        } else {
            Draft::Composer(format!("/btw {question}"))
        };
        self.submit_with_receipt(
            Request::Ask {
                session: self.id.clone().unwrap_or_default(),
                question,
                follow_up,
            },
            draft,
        );
    }

    fn rejected(&mut self, draft: Draft, error: anyhow::Error) {
        match draft {
            Draft::Plan => self.state.planning.failed(error.to_string()),
            Draft::Model(_) | Draft::Permissions(_) => self
                .state
                .report_local(TranscriptItem::Error(error.to_string())),
            Draft::UserInput(id) => self
                .state
                .user_input
                .submission_error(&id, error.to_string()),
            Draft::Question => self.state.btw.submit_error(&error),
            Draft::Composer(text) => {
                if self.state.input.text.is_empty() {
                    self.state.input.set(text);
                } else {
                    self.state
                        .report_local(TranscriptItem::Error(format!("尚未确认的输入：{text}")));
                }
                self.state
                    .report_local(TranscriptItem::Error(error.to_string()));
            }
        }
    }

    pub(super) fn poll_submissions(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.submissions.len() {
            let result = match self.submissions[index].response.try_recv() {
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
            changed = true;
            match result {
                Ok(Response::Profile(info))
                    if matches!(submission.draft, Draft::Permissions(_)) =>
                {
                    self.state.permissions.apply(info.mode, info.mode_revision);
                }
                Ok(Response::Ok) if !matches!(submission.draft, Draft::Permissions(_)) => {
                    if matches!(submission.draft, Draft::Plan) {
                        self.state.planning.succeeded();
                    }
                    if let Draft::UserInput(id) = &submission.draft {
                        self.state.user_input.submission_succeeded(id);
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
                        self.state.report_local(TranscriptItem::Error(format!(
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
        let pending = std::mem::take(&mut self.submissions);
        for submission in pending {
            self.rejected(
                submission.draft,
                anyhow::anyhow!("连接已断开，请重连后确认请求结果"),
            );
        }
        self.sync_pending_permissions();
    }

    fn sync_pending_permissions(&mut self) {
        self.state.permissions.pending =
            self.submissions
                .iter()
                .rev()
                .find_map(|submission| match submission.draft {
                    Draft::Permissions(mode) => Some(mode),
                    _ => None,
                });
    }

    pub(super) fn poll_questions(&mut self) -> bool {
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
            && (self.state.btw.visible() || self.state.btw.running())
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
