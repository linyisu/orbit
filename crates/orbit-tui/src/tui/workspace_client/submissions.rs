use tokio::sync::oneshot;

use super::*;

pub(super) enum Draft {
    Composer(String),
    Question,
}

pub(super) struct Submission {
    response: oneshot::Receiver<Result<Response>>,
    draft: Draft,
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
    }

    pub(super) fn ask(&mut self, question: String, follow_up: Option<usize>) {
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
            Draft::Question => self.state.btw.submit_error(&error),
            Draft::Composer(text) => {
                if self.state.input.text.is_empty() {
                    self.state.input.set(text);
                } else {
                    self.state
                        .report_local(TranscriptItem::Notice(format!("尚未确认的输入：{text}")));
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
                Ok(Response::Ok) => {}
                Ok(response @ Response::Questions(_)) => self.apply(response),
                Ok(Response::Error(error)) => {
                    self.rejected(submission.draft, anyhow::anyhow!(error))
                }
                Err(error) => self.rejected(submission.draft, error),
                Ok(_) => self.rejected(submission.draft, anyhow::anyhow!("工作区响应类型不匹配")),
            }
        }
        changed
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
