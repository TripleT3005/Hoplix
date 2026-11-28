use std::sync::Arc;

use crate::{BoxFuture, Error, ErrorKind, Result};

// Constants

pub const PROMPT_CANCELLED: &str = "the sign-in was cancelled";
pub const PROMPT_UNAVAILABLE: &str = "the server asks for input that nobody can answer here";
const MAX_REQUESTS: usize = 16;
const MAX_ROUNDS: usize = 8;
const MAX_KEYBOARD_ATTEMPTS: usize = 3;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptField {
    pub text: String,
    pub echo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRequest {
    pub host: String,
    pub jump: bool,
    pub name: String,
    pub instruction: String,
    pub fields: Vec<PromptField>,
    pub repeated: bool,
}

pub trait Prompter: Send + Sync {
    fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>>;
}

pub type SharedPrompter = Arc<dyn Prompter>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Methods {
    pub known: bool,
    pub password: bool,
    pub keyboard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure { methods: Methods, partial: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Done(Outcome),
    Ask {
        name: String,
        instruction: String,
        fields: Vec<PromptField>,
    },
}

pub(crate) trait AuthChannel: Send {
    fn probe(&mut self) -> BoxFuture<'_, Result<Outcome>>;

    fn primary(&mut self) -> BoxFuture<'_, Result<Outcome>>;

    fn password(&mut self, password: String) -> BoxFuture<'_, Result<Outcome>>;

    fn keyboard_start(&mut self) -> BoxFuture<'_, Result<Step>>;

    fn keyboard_respond(&mut self, answers: Vec<String>) -> BoxFuture<'_, Result<Step>>;
}

pub(crate) struct SignIn<'a> {
    pub label: &'a str,
    pub jump: bool,
    pub password: Option<&'a str>,
    pub prompter: Option<&'a SharedPrompter>,
}

struct Rounds<'a> {
    autofill: Option<&'a str>,
    answered: Option<Vec<PromptField>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Primary,
    Password,
    Keyboard,
}

// Text

pub fn is_password_prompt(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    let text = text.strip_suffix(':').unwrap_or(&text).trim_end();
    if text == "password" {
        return true;
    }
    text.strip_suffix("'s password")
        .is_some_and(|owner| !owner.is_empty() && !owner.contains(char::is_whitespace))
}

pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn clean_fields(fields: &[PromptField]) -> Vec<PromptField> {
    fields
        .iter()
        .map(|field| PromptField {
            text: clean_text(&field.text),
            echo: field.echo,
        })
        .collect()
}

impl PromptRequest {
    fn new(
        sign_in: &SignIn<'_>,
        name: &str,
        instruction: &str,
        fields: Vec<PromptField>,
        repeated: bool,
    ) -> Self {
        Self {
            host: sign_in.label.to_string(),
            jump: sign_in.jump,
            name: clean_text(name),
            instruction: clean_text(instruction),
            fields,
            repeated,
        }
    }
}

// Signing in

fn failed(label: &str) -> Error {
    Error::new(ErrorKind::AuthenticationFailed, label)
}

pub(crate) async fn sign_in(channel: &mut impl AuthChannel, context: &SignIn<'_>) -> Result<()> {
    let offered = match channel.probe().await? {
        Outcome::Success => return Ok(()),
        Outcome::Failure { methods, .. } => methods,
    };
    let mut rounds = Rounds {
        autofill: context.password.filter(|password| !password.is_empty()),
        answered: None,
    };
    let mut keyboard_attempts = 0;
    let (mut outcome, mut last) = match context.password {
        Some(password) if prefers_keyboard(password, offered) => {
            keyboard_attempts += 1;
            let outcome = keyboard(channel, context, &mut rounds).await?;
            (outcome, Last::Keyboard)
        }
        Some(password) => {
            rounds.autofill = None;
            let outcome = channel.password(password.to_string()).await?;
            (outcome, Last::Password)
        }
        None => (channel.primary().await?, Last::Primary),
    };
    let mut password_used = last == Last::Password;
    for _ in 0..MAX_ROUNDS {
        let Outcome::Failure { methods, partial } = outcome else {
            return Ok(());
        };
        let retry = last == Last::Keyboard && keyboard_attempts < MAX_KEYBOARD_ATTEMPTS;
        if methods.keyboard && (partial || retry) {
            keyboard_attempts += 1;
            outcome = keyboard(channel, context, &mut rounds).await?;
            last = Last::Keyboard;
        } else if partial && methods.password && !password_used {
            let Some(password) = context.password.filter(|password| !password.is_empty()) else {
                return Err(failed(context.label));
            };
            password_used = true;
            rounds.autofill = None;
            outcome = channel.password(password.to_string()).await?;
            last = Last::Password;
        } else {
            return Err(failed(context.label));
        }
    }
    Err(failed(context.label))
}

fn prefers_keyboard(password: &str, offered: Methods) -> bool {
    offered.known && offered.keyboard && (password.is_empty() || !offered.password)
}

async fn keyboard(
    channel: &mut impl AuthChannel,
    sign_in: &SignIn<'_>,
    rounds: &mut Rounds<'_>,
) -> Result<Outcome> {
    let mut step = channel.keyboard_start().await?;
    for _ in 0..MAX_REQUESTS {
        let (name, instruction, fields) = match step {
            Step::Done(outcome) => return Ok(outcome),
            Step::Ask {
                name,
                instruction,
                fields,
            } => (name, instruction, fields),
        };
        if fields.is_empty() {
            step = channel.keyboard_respond(Vec::new()).await?;
            continue;
        }
        let fields = clean_fields(&fields);
        let repeated = rounds.answered.as_ref() == Some(&fields);
        let answers = if let Some(password) = rounds.autofill.filter(|_| can_autofill(&fields)) {
            rounds.autofill = None;
            vec![password.to_string()]
        } else {
            rounds.autofill = None;
            let Some(prompter) = sign_in.prompter else {
                return Err(Error::new(
                    ErrorKind::AuthenticationFailed,
                    format!("{}: {PROMPT_UNAVAILABLE}", sign_in.label),
                ));
            };
            let count = fields.len();
            let request =
                PromptRequest::new(sign_in, &name, &instruction, fields.clone(), repeated);
            match prompter.answer(request).await {
                Some(answers) if answers.len() == count => answers,
                Some(_) => return Err(failed(sign_in.label)),
                None => {
                    return Err(Error::new(
                        ErrorKind::AuthenticationFailed,
                        format!("{}: {PROMPT_CANCELLED}", sign_in.label),
                    ));
                }
            }
        };
        rounds.answered = Some(fields);
        step = channel.keyboard_respond(answers).await?;
    }
    Err(failed(sign_in.label))
}

fn can_autofill(fields: &[PromptField]) -> bool {
    matches!(fields, [field] if !field.echo && is_password_prompt(&field.text))
}

pub fn is_cancelled(error: &Error) -> bool {
    error.kind() == ErrorKind::AuthenticationFailed && error.detail().ends_with(PROMPT_CANCELLED)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::test_support::test_password;

    // Mocks

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Probe,
        Primary,
        Password(String),
        Start,
        Respond(Vec<String>),
    }

    struct Script {
        probe: Outcome,
        primary: Outcome,
        password: VecDeque<Outcome>,
        steps: VecDeque<Step>,
        calls: Vec<Call>,
    }

    impl Script {
        fn new(offered: Methods) -> Self {
            Self {
                probe: Outcome::Failure {
                    methods: offered,
                    partial: false,
                },
                primary: Outcome::Success,
                password: VecDeque::new(),
                steps: VecDeque::new(),
                calls: Vec::new(),
            }
        }

        fn next_step(&mut self) -> Step {
            self.steps
                .pop_front()
                .unwrap_or(Step::Done(Outcome::Failure {
                    methods: Methods::default(),
                    partial: false,
                }))
        }
    }

    impl AuthChannel for Script {
        fn probe(&mut self) -> BoxFuture<'_, Result<Outcome>> {
            self.calls.push(Call::Probe);
            let outcome = self.probe;
            Box::pin(async move { Ok(outcome) })
        }

        fn primary(&mut self) -> BoxFuture<'_, Result<Outcome>> {
            self.calls.push(Call::Primary);
            let outcome = self.primary;
            Box::pin(async move { Ok(outcome) })
        }

        fn password(&mut self, password: String) -> BoxFuture<'_, Result<Outcome>> {
            self.calls.push(Call::Password(password));
            let outcome = self.password.pop_front().unwrap_or(Outcome::Success);
            Box::pin(async move { Ok(outcome) })
        }

        fn keyboard_start(&mut self) -> BoxFuture<'_, Result<Step>> {
            self.calls.push(Call::Start);
            let step = self.next_step();
            Box::pin(async move { Ok(step) })
        }

        fn keyboard_respond(&mut self, answers: Vec<String>) -> BoxFuture<'_, Result<Step>> {
            self.calls.push(Call::Respond(answers));
            let step = self.next_step();
            Box::pin(async move { Ok(step) })
        }
    }

    struct Answers {
        replies: Mutex<VecDeque<Option<Vec<String>>>>,
        requests: Mutex<Vec<PromptRequest>>,
    }

    impl Answers {
        fn new(replies: Vec<Option<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<PromptRequest> {
            self.requests.lock().expect("poisoned").clone()
        }
    }

    impl Prompter for Answers {
        fn answer(&self, request: PromptRequest) -> BoxFuture<'_, Option<Vec<String>>> {
            self.requests.lock().expect("poisoned").push(request);
            let reply = self.replies.lock().expect("poisoned").pop_front().flatten();
            Box::pin(async move { reply })
        }
    }

    const BOTH: Methods = Methods {
        known: true,
        password: true,
        keyboard: true,
    };
    const KEYBOARD: Methods = Methods {
        known: true,
        password: false,
        keyboard: true,
    };

    fn field(text: &str, echo: bool) -> PromptField {
        PromptField {
            text: text.to_string(),
            echo,
        }
    }

    fn ask(fields: Vec<PromptField>) -> Step {
        Step::Ask {
            name: String::new(),
            instruction: "Use your token\u{1b}[2J".to_string(),
            fields,
        }
    }

    fn partial(methods: Methods) -> Outcome {
        Outcome::Failure {
            methods,
            partial: true,
        }
    }

    fn shared(answers: &Arc<Answers>) -> SharedPrompter {
        Arc::clone(answers) as SharedPrompter
    }

    async fn run(
        script: &mut Script,
        password: Option<&str>,
        prompter: &SharedPrompter,
    ) -> Result<()> {
        let sign_in = SignIn {
            label: "demo@example.test",
            jump: false,
            password,
            prompter: Some(prompter),
        };
        super::sign_in(script, &sign_in).await
    }

    // Flows

    #[tokio::test]
    async fn a_key_with_partial_success_asks_for_the_code() {
        let mut script = Script::new(BOTH);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([
            ask(vec![field("Verification code: ", false)]),
            Step::Done(Outcome::Success),
        ]);
        let code = test_password();
        let answers = Answers::new(vec![Some(vec![code.clone()])]);
        assert_eq!(run(&mut script, None, &shared(&answers)).await, Ok(()));
        assert_eq!(
            script.calls,
            [
                Call::Probe,
                Call::Primary,
                Call::Start,
                Call::Respond(vec![code])
            ]
        );
        let requests = answers.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].host, "demo@example.test");
        assert_eq!(requests[0].instruction, "Use your token[2J");
        assert_eq!(requests[0].fields, [field("Verification code:", false)]);
        assert!(!requests[0].repeated);
    }

    #[tokio::test]
    async fn the_saved_password_answers_a_password_prompt_once() {
        let password = test_password();
        let code = test_password();
        let mut script = Script::new(KEYBOARD);
        script.steps = VecDeque::from([
            ask(vec![field("Password: ", false)]),
            ask(vec![field("Password: ", false)]),
            ask(vec![field("Verification code: ", true)]),
            Step::Done(Outcome::Success),
        ]);
        let typed = test_password();
        let answers = Answers::new(vec![Some(vec![typed.clone()]), Some(vec![code.clone()])]);
        assert_eq!(
            run(&mut script, Some(&password), &shared(&answers)).await,
            Ok(())
        );
        assert_eq!(
            script.calls,
            [
                Call::Probe,
                Call::Start,
                Call::Respond(vec![password]),
                Call::Respond(vec![typed]),
                Call::Respond(vec![code]),
            ]
        );
        let repeated: Vec<bool> = answers.requests().iter().map(|r| r.repeated).collect();
        assert_eq!(repeated, [true, false]);
    }

    #[tokio::test]
    async fn a_second_step_with_other_questions_is_no_repeat() {
        let mut script = Script::new(KEYBOARD);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([
            ask(vec![field("Password: ", false)]),
            ask(vec![field("Verification code: ", false)]),
            ask(vec![field("Verification code: ", false)]),
            Step::Done(Outcome::Success),
        ]);
        let replies = (0..3).map(|_| Some(vec![test_password()])).collect();
        let answers = Answers::new(replies);
        assert_eq!(run(&mut script, None, &shared(&answers)).await, Ok(()));
        let repeated: Vec<bool> = answers.requests().iter().map(|r| r.repeated).collect();
        assert_eq!(repeated, [false, false, true]);
    }

    #[tokio::test]
    async fn the_password_method_is_used_when_offered() {
        let password = test_password();
        let mut script = Script::new(BOTH);
        let answers = Answers::new(Vec::new());
        assert_eq!(
            run(&mut script, Some(&password), &shared(&answers)).await,
            Ok(())
        );
        assert_eq!(script.calls, [Call::Probe, Call::Password(password)]);
        assert!(answers.requests().is_empty());
    }

    #[tokio::test]
    async fn an_empty_password_goes_straight_to_the_prompts() {
        let empty = String::new();
        let code = test_password();
        let mut script = Script::new(BOTH);
        script.steps = VecDeque::from([
            ask(vec![field("Password: ", false)]),
            Step::Done(Outcome::Success),
        ]);
        let answers = Answers::new(vec![Some(vec![code.clone()])]);
        assert_eq!(
            run(&mut script, Some(&empty), &shared(&answers)).await,
            Ok(())
        );
        assert_eq!(
            script.calls,
            [Call::Probe, Call::Start, Call::Respond(vec![code])]
        );
    }

    #[tokio::test]
    async fn wrong_codes_are_asked_again_a_few_times() {
        let mut script = Script::new(BOTH);
        script.primary = partial(KEYBOARD);
        let wrong = Outcome::Failure {
            methods: KEYBOARD,
            partial: false,
        };
        script.steps = (0..MAX_KEYBOARD_ATTEMPTS)
            .flat_map(|_| [ask(vec![field("Code: ", false)]), Step::Done(wrong)])
            .collect();
        let replies = (0..MAX_KEYBOARD_ATTEMPTS)
            .map(|_| Some(vec![test_password()]))
            .collect();
        let answers = Answers::new(replies);
        let error = run(&mut script, None, &shared(&answers))
            .await
            .expect_err("signed in");
        assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
        assert!(!is_cancelled(&error));
        let requests = answers.requests();
        assert_eq!(requests.len(), MAX_KEYBOARD_ATTEMPTS);
        assert!(!requests[0].repeated);
        assert!(requests[1..].iter().all(|request| request.repeated));
    }

    #[tokio::test]
    async fn cancelling_stops_the_sign_in() {
        let mut script = Script::new(BOTH);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([ask(vec![field("Code: ", false)])]);
        let answers = Answers::new(vec![None]);
        let error = run(&mut script, None, &shared(&answers))
            .await
            .expect_err("signed in");
        assert!(is_cancelled(&error));
        assert_eq!(script.calls, [Call::Probe, Call::Primary, Call::Start]);
    }

    #[tokio::test]
    async fn without_a_prompter_the_error_says_so() {
        let mut script = Script::new(KEYBOARD);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([ask(vec![field("Code: ", false)])]);
        let sign_in = SignIn {
            label: "hop@bastion.test",
            jump: true,
            password: None,
            prompter: None,
        };
        let error = super::sign_in(&mut script, &sign_in)
            .await
            .expect_err("signed in");
        assert_eq!(
            error.detail(),
            format!("hop@bastion.test: {PROMPT_UNAVAILABLE}")
        );
    }

    #[tokio::test]
    async fn jump_hosts_are_named_and_empty_requests_answered_silently() {
        let mut script = Script::new(BOTH);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([
            ask(Vec::new()),
            ask(vec![field("Code: ", false), field("User: ", true)]),
            Step::Done(Outcome::Success),
        ]);
        let answers = Answers::new(vec![Some(vec![test_password(), "demo".to_string()])]);
        let prompter = shared(&answers);
        let sign_in = SignIn {
            label: "hop@bastion.test",
            jump: true,
            password: None,
            prompter: Some(&prompter),
        };
        assert_eq!(super::sign_in(&mut script, &sign_in).await, Ok(()));
        assert_eq!(script.calls[3], Call::Respond(Vec::new()));
        let request = &answers.requests()[0];
        assert!(request.jump);
        assert_eq!(request.host, "hop@bastion.test");
        assert_eq!(request.fields.len(), 2);
    }

    #[tokio::test]
    async fn wrong_answer_counts_and_plain_failures_fail() {
        let mut script = Script::new(BOTH);
        script.primary = partial(KEYBOARD);
        script.steps = VecDeque::from([ask(vec![field("Code: ", false)])]);
        let answers = Answers::new(vec![Some(Vec::new())]);
        assert!(run(&mut script, None, &shared(&answers)).await.is_err());

        let mut script = Script::new(BOTH);
        script.primary = Outcome::Failure {
            methods: BOTH,
            partial: false,
        };
        let answers = Answers::new(Vec::new());
        let error = run(&mut script, None, &shared(&answers))
            .await
            .expect_err("signed in");
        assert_eq!(error.detail(), "demo@example.test");
        assert_eq!(script.calls, [Call::Probe, Call::Primary]);
    }

    #[tokio::test]
    async fn a_partial_success_without_further_methods_fails() {
        let password = test_password();
        let mut script = Script::new(BOTH);
        script.password = VecDeque::from([partial(Methods {
            known: true,
            password: false,
            keyboard: false,
        })]);
        let answers = Answers::new(Vec::new());
        let error = run(&mut script, Some(&password), &shared(&answers))
            .await
            .expect_err("signed in");
        assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
        assert_eq!(script.calls, [Call::Probe, Call::Password(password)]);
    }

    // Text

    #[test]
    fn password_prompts_are_recognised_strictly() {
        for text in [
            "Password:",
            "password: ",
            " PASSWORD",
            "demo@host's password: ",
        ] {
            assert!(is_password_prompt(text), "{text:?}");
        }
        for text in [
            "One-time password:",
            "Verification code:",
            "New password:",
            "demo user's password:",
            "'s password:",
        ] {
            assert!(!is_password_prompt(text), "{text:?}");
        }
        assert_eq!(clean_text("a\u{7}b\n\tc\r\n"), "ab\n\tc");
    }
}
