use std::collections::VecDeque;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use hoplix_core::{BoxFuture, PromptRequest, Prompter};
use iced::futures::channel::{mpsc, oneshot};
use iced::futures::lock::Mutex as AsyncMutex;
use iced::futures::{SinkExt, StreamExt};
use iced::{Subscription, Task};

use crate::app::{Dialog, Hoplix, Message};

// Constants

pub const FIRST_INPUT_ID: &str = "prompt-input";
const EVENT_BUFFER: usize = 8;

// Types

type Reply = oneshot::Sender<Option<Vec<String>>>;

#[derive(Clone)]
pub struct PromptTicket {
    reply: Arc<Mutex<Option<Reply>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDialog {
    pub request: PromptRequest,
    pub answers: Vec<String>,
    ticket: PromptTicket,
}

#[derive(Debug, Clone)]
pub enum PromptMessage {
    Requested(Box<PromptDialog>),
    Input(usize, String),
}

#[derive(Debug, Default)]
pub struct PromptQueue {
    waiting: VecDeque<PromptDialog>,
}

#[derive(Clone)]
pub struct PromptBroker {
    sender: mpsc::UnboundedSender<(PromptRequest, Reply)>,
    receiver: Arc<AsyncMutex<mpsc::UnboundedReceiver<(PromptRequest, Reply)>>>,
}

// Tickets

impl PromptTicket {
    fn new(reply: Reply) -> Self {
        Self {
            reply: Arc::new(Mutex::new(Some(reply))),
        }
    }

    fn send(&self, answers: Option<Vec<String>>) {
        if let Some(reply) = self.reply.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = reply.send(answers);
        }
    }
}

impl fmt::Debug for PromptTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PromptTicket")
    }
}

impl PartialEq for PromptTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

impl Eq for PromptTicket {}

impl PromptDialog {
    #[cfg(test)]
    pub fn new(request: PromptRequest) -> (Self, oneshot::Receiver<Option<Vec<String>>>) {
        let (reply, answer) = oneshot::channel();
        let answers = vec![String::new(); request.fields.len()];
        let dialog = Self {
            request,
            answers,
            ticket: PromptTicket::new(reply),
        };
        (dialog, answer)
    }

    fn from_reply(request: PromptRequest, reply: Reply) -> Self {
        let answers = vec![String::new(); request.fields.len()];
        Self {
            request,
            answers,
            ticket: PromptTicket::new(reply),
        }
    }
}

impl fmt::Debug for PromptBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PromptBroker")
    }
}

// Broker

impl Default for PromptBroker {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            sender,
            receiver: Arc::new(AsyncMutex::new(receiver)),
        }
    }
}

impl Prompter for PromptBroker {
    fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
        let (reply, answer) = oneshot::channel();
        let sent = self.sender.unbounded_send((request, reply)).is_ok();
        Box::pin(async move {
            if !sent {
                return None;
            }
            answer.await.ok().flatten()
        })
    }
}

impl Hash for PromptBroker {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "prompt-broker".hash(state);
    }
}

pub fn subscription(app: &Hoplix) -> Subscription<Message> {
    Subscription::run_with(app.services.prompts.clone(), prompt_requests)
}

fn prompt_requests(broker: &PromptBroker) -> impl iced::futures::Stream<Item = Message> + use<> {
    let receiver = Arc::clone(&broker.receiver);
    iced::stream::channel(EVENT_BUFFER, async move |mut output| {
        let mut receiver = receiver.lock().await;
        while let Some((request, reply)) = receiver.next().await {
            let dialog = PromptDialog::from_reply(request, reply);
            let message = Message::Prompt(PromptMessage::Requested(Box::new(dialog)));
            if output.send(message).await.is_err() {
                break;
            }
        }
    })
}

// Dialogs

impl Hoplix {
    pub(crate) fn update_prompt(&mut self, message: PromptMessage) -> Task<Message> {
        match message {
            PromptMessage::Requested(dialog) => {
                self.prompts.waiting.push_back(*dialog);
                return self.show_next_prompt();
            }
            PromptMessage::Input(index, value) => {
                if let Some(Dialog::Prompt(dialog)) = &mut self.dialog
                    && let Some(answer) = dialog.answers.get_mut(index)
                {
                    *answer = value;
                }
            }
        }
        Task::none()
    }

    pub(crate) fn show_next_prompt(&mut self) -> Task<Message> {
        if self.dialog.is_some() {
            return Task::none();
        }
        let Some(next) = self.prompts.waiting.pop_front() else {
            return Task::none();
        };
        self.dialog = Some(Dialog::Prompt(Box::new(next)));
        iced::widget::operation::focus(FIRST_INPUT_ID)
    }

    pub(crate) fn has_waiting_prompts(&self) -> bool {
        !self.prompts.waiting.is_empty()
    }

    pub(crate) fn answer_prompt(&mut self, confirmed: bool) -> Task<Message> {
        let Some(Dialog::Prompt(dialog)) = self.dialog.take() else {
            return Task::none();
        };
        let PromptDialog {
            answers, ticket, ..
        } = *dialog;
        ticket.send(confirmed.then_some(answers));
        self.show_next_prompt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoplix_core::PromptField;

    fn request() -> PromptRequest {
        PromptRequest {
            host: "demo@example.test".to_string(),
            jump: false,
            name: String::new(),
            instruction: String::new(),
            fields: vec![PromptField {
                text: "Code:".to_string(),
                echo: false,
            }],
        }
    }

    #[test]
    fn tickets_answer_once_and_compare_by_identity() {
        let (dialog, mut answer) = PromptDialog::new(request());
        let copy = dialog.clone();
        assert_eq!(dialog, copy);
        let (other, _) = PromptDialog::new(request());
        assert_ne!(dialog, other);
        dialog.ticket.send(Some(vec!["1".to_string()]));
        copy.ticket.send(None);
        assert_eq!(answer.try_recv(), Ok(Some(Some(vec!["1".to_string()]))));
    }

    #[test]
    fn requests_reach_the_subscription_and_answers_flow_back() {
        let broker = PromptBroker::default();
        let answered = iced_test::futures::futures::executor::block_on(async {
            let mut stream = std::pin::pin!(prompt_requests(&broker));
            let asking = broker.answer(request());
            let replying = async {
                let Some(Message::Prompt(PromptMessage::Requested(dialog))) = stream.next().await
                else {
                    panic!("no prompt arrived");
                };
                assert_eq!(dialog.request, request());
                assert_eq!(dialog.answers, [String::new()]);
                dialog.ticket.send(Some(vec!["42".to_string()]));
            };
            iced::futures::join!(asking, replying).0
        });
        assert_eq!(answered, Some(vec!["42".to_string()]));
    }

    #[test]
    fn a_dropped_request_answers_nothing() {
        let broker = PromptBroker::default();
        let answered = iced_test::futures::futures::executor::block_on(async {
            let answer = broker.answer(request());
            let mut receiver = broker.receiver.lock().await;
            let (sent, reply) = receiver.next().await.expect("nothing was asked");
            assert_eq!(sent, request());
            drop(reply);
            answer.await
        });
        assert_eq!(answered, None);
    }
}
