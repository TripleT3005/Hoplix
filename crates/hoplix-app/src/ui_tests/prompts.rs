use hoplix_core::{Conflict, PromptField, PromptRequest, Protocol};
use iced::futures::channel::oneshot;
use iced::keyboard::key::Named;

use super::*;
use crate::app::Dialog;
use crate::prompts::{PromptDialog, PromptMessage};

// Helpers

type Answer = oneshot::Receiver<Option<Vec<String>>>;

fn request(host: &str, jump: bool, fields: &[(&str, bool)]) -> PromptRequest {
    PromptRequest {
        host: host.to_string(),
        jump,
        name: String::new(),
        instruction: "Open your authenticator app".to_string(),
        fields: fields
            .iter()
            .map(|(text, echo)| PromptField {
                text: (*text).to_string(),
                echo: *echo,
            })
            .collect(),
    }
}

fn ask(harness: &mut Harness, request: PromptRequest) -> Answer {
    let (dialog, answer) = PromptDialog::new(request);
    harness.send(Message::Prompt(PromptMessage::Requested(Box::new(dialog))));
    answer
}

fn answered(answer: &mut Answer) -> Option<Option<Vec<String>>> {
    answer.try_recv().expect("the prompt was dropped")
}

// Dialogs

#[test]
fn prompts_show_the_server_text_and_return_the_answers() {
    let mut harness = Harness::new("prompt-answers");
    let fields = [("Verification code: ", false), ("Device name: ", true)];
    let mut answer = ask(
        &mut harness,
        request("demo@files.example.test", false, &fields),
    );
    assert!(matches!(harness.app.dialog, Some(Dialog::Prompt(_))));
    assert!(harness.has("demo@files.example.test asks for sign-in details"));
    assert!(harness.has("Open your authenticator app"));
    assert!(harness.has("Verification code:"));
    assert!(harness.has("Device name:"));
    assert!(harness.contains("Hoplix does not save the answers"));
    assert!(!harness.contains("Save"));

    let code = test_password();
    harness.type_text(&code);
    let inputs = harness.inputs();
    let device = inputs[inputs.len() - 1];
    harness.fill_at(device.center(), "laptop");
    assert!(answer.try_recv().expect("dropped").is_none());
    harness.click("Sign in");
    assert!(harness.app.dialog.is_none());
    assert_eq!(
        answered(&mut answer),
        Some(Some(vec![code.clone(), "laptop".to_string()]))
    );
    assert!(!harness.contains(&code));
}

#[test]
fn jump_host_prompts_name_the_hop_and_enter_confirms() {
    let mut harness = Harness::new("prompt-jump");
    let mut answer = ask(
        &mut harness,
        request("hop@bastion.example.test:2222", true, &[("Code:", false)]),
    );
    assert!(harness.has("The jump host hop@bastion.example.test:2222 asks for sign-in details"));
    harness.type_text("123456");
    harness.key(Named::Enter);
    assert_eq!(
        answered(&mut answer),
        Some(Some(vec!["123456".to_string()]))
    );
}

#[test]
fn cancelling_answers_nothing_and_queued_prompts_follow() {
    let mut harness = Harness::new("prompt-cancel");
    let mut first = ask(
        &mut harness,
        request("demo@one.test", false, &[("Code:", false)]),
    );
    let mut second = ask(
        &mut harness,
        request("demo@two.test", false, &[("Code:", false)]),
    );
    assert!(harness.has("demo@one.test asks for sign-in details"));
    assert!(!harness.has("demo@two.test asks for sign-in details"));
    harness.click("Cancel");
    assert_eq!(answered(&mut first), Some(None));
    assert!(harness.has("demo@two.test asks for sign-in details"));
    harness.key(Named::Escape);
    if harness.app.dialog.is_some() {
        harness.click("Cancel");
    }
    assert_eq!(answered(&mut second), Some(None));
    assert!(harness.app.dialog.is_none());
}

#[test]
fn prompts_wait_for_an_open_dialog() {
    let mut harness = Harness::new("prompt-wait");
    harness.app.dialog = Some(Dialog::NewSiteFolder {
        parent: String::new(),
        name: String::new(),
    });
    let mut answer = ask(
        &mut harness,
        request("demo@files.example.test", false, &[("Code:", false)]),
    );
    assert!(matches!(
        harness.app.dialog,
        Some(Dialog::NewSiteFolder { .. })
    ));
    harness.send(Message::DialogCancelled);
    assert!(matches!(harness.app.dialog, Some(Dialog::Prompt(_))));
    harness.click("Cancel");
    assert_eq!(answered(&mut answer), Some(None));
}

// Protocols

#[test]
fn scp_can_be_chosen_and_explains_its_limits() {
    let mut harness = Harness::start("prompt-scp", Layout::Modern, |_| {});
    harness.click("Connect");
    harness.pick("Protocol", 4);
    assert_eq!(harness.app.connect_form.protocol, Protocol::Scp);
    assert!(harness.contains("SCP needs a POSIX shell"));
    assert!(harness.has("Sign in with"));
    harness.app.connect_form.jump.enabled = true;
    assert!(harness.app.connect_form.uses_jump());
}

#[test]
fn conflicts_offer_resume_only_where_it_works() {
    let mut harness = Harness::new("prompt-resume");
    for can_resume in [true, false] {
        harness.app.dialog = Some(Dialog::Conflict {
            conflict: Conflict {
                job: 1,
                target_path: "/srv/a.bin".to_string(),
                source_size: 10,
                target_size: 4,
                can_resume,
            },
            apply_to_all: false,
        });
        assert!(harness.has("Overwrite"));
        assert_eq!(harness.has("Resume"), can_resume);
        harness.app.dialog = None;
    }
}
