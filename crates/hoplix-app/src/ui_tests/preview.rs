use iced::keyboard::key::Named;

use super::*;
use crate::app::{TabId, TabMessage};
use crate::copy_path::LINE_BREAK;
use crate::preview::{Content, PREVIEW_BYTES, PreviewMessage, limit_bytes};
use crate::transfers::DropTarget;

// Constants

const SERVER: &str = "demo@files.example.test";
const NOTES: &str = "first line of the notes\nsecond line";

// Helpers

fn local(name: &str) -> Harness {
    Harness::start(name, Layout::Modern, |root| {
        fs::write(root.data().join("notes.txt"), NOTES).expect("writing the notes failed");
        write_files(&root.data(), &["photo.png", "docs/"]);
    })
}

fn connected(name: &str, prepare: impl FnOnce(&TestRoot)) -> Harness {
    let mut harness = Harness::start(name, Layout::Modern, prepare);
    let server = fakes::Folder::remote(&harness.root.remote(), SERVER.to_string());
    harness.send(Message::Connected(Ok(server)));
    assert!(harness.tab().file_system.label().is_some());
    harness
}

fn folder(name: &str) -> String {
    format!("{name}{}", std::path::MAIN_SEPARATOR)
}

fn content(harness: &Harness) -> Content {
    harness.app.preview.content.clone()
}

fn shown_text(harness: &Harness) -> String {
    match content(harness) {
        Content::Text(preview) => preview.text,
        other => panic!("no text preview: {other:?}"),
    }
}

fn index_of(harness: &Harness, name: &str) -> usize {
    harness
        .tab()
        .entries
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} is not listed"))
}

fn tab_id(harness: &Harness) -> TabId {
    harness.tab().id
}

fn finished(app: &Hoplix) -> bool {
    !app.transfer_jobs.is_empty() && app.transfer_jobs.iter().all(|job| job.status.is_finished())
}

fn drop_files(harness: &mut Harness, paths: &[PathBuf]) {
    for path in paths {
        harness.send(Message::Window(
            harness.window,
            window::Event::FileHovered(path.clone()),
        ));
    }
    for path in paths {
        harness.send(Message::Window(
            harness.window,
            window::Event::FileDropped(path.clone()),
        ));
    }
}

fn outside_files(harness: &Harness, names: &[&str]) -> Vec<PathBuf> {
    let folder = harness.root.temp().join("outside");
    write_files(&folder, names);
    names
        .iter()
        .map(|name| folder.join(name.trim_end_matches('/')))
        .collect()
}

// Toggling

#[test]
fn f3_toggles_the_preview_pane_and_the_session_keeps_it() {
    let mut harness = local("preview-toggle");
    assert!(!harness.has("Select a file to preview it."));
    harness.key(Named::F3);
    assert!(harness.app.preview.open);
    assert!(harness.has("Select a file to preview it."));
    harness.key(Named::F3);
    assert!(!harness.app.preview.open);
    assert!(!harness.has("Select a file to preview it."));

    harness.click("Preview (F3)");
    assert!(harness.app.preview.open);
    harness.app.preview.width = 450.0;
    let window = harness.window;
    harness.send(Message::WindowClosing(window, false));
    harness.restart();
    assert!(harness.app.preview.open);
    assert_eq!(harness.app.preview.width, 450.0);
}

// Local files

#[test]
fn selecting_a_local_text_file_shows_its_content() {
    let mut harness = local("preview-local");
    harness.key(Named::F3);
    harness.click("notes.txt");
    assert_eq!(shown_text(&harness), NOTES);
    assert!(harness.has(NOTES));
    assert!(harness.has("2 lines · UTF-8"));
    assert!(!harness.contains("Only the first"));

    harness.click("photo.png");
    assert_eq!(content(&harness), Content::NoPreview);
    assert!(harness.has("No preview for this type of file."));
    harness.click(&folder("docs"));
    assert_eq!(content(&harness), Content::Folder);
    harness.shortcut("a", Modifiers::CTRL);
    assert_eq!(content(&harness), Content::Several(3));
    assert!(harness.has("3 items selected."));
}

// Remote files

#[test]
fn remote_files_under_the_limit_load_and_larger_ones_wait_for_a_click() {
    let mut harness = connected("preview-remote", |root| {
        write_files(&root.remote(), &["small.txt"]);
        let large = vec![b'x'; usize::try_from(limit_bytes(2)).unwrap_or(0) + 1];
        fs::write(root.remote().join("large.log"), large).expect("writing the large file failed");
    });
    harness.key(Named::F3);
    harness.click("small.txt");
    assert_eq!(shown_text(&harness), "small.txt");

    harness.click("large.log");
    assert_eq!(content(&harness), Content::Confirm);
    assert!(harness.has("Load preview"));
    harness.click("Load preview");
    let text = shown_text(&harness);
    assert_eq!(text.len(), PREVIEW_BYTES);
    assert!(harness.has("Only the first 256.0 KiB are shown."));
}

#[test]
fn changing_the_selection_cancels_the_pending_preview() {
    let mut harness = connected("preview-cancel", |root| {
        write_files(&root.remote(), &["first.txt", "second.txt"]);
    });
    harness.key(Named::F3);
    let id = tab_id(&harness);
    let first = index_of(&harness, "first.txt");
    let pending = harness
        .app
        .update(Message::Tab(id, TabMessage::Select(first)));
    let stale = harness.app.preview.generation();
    assert_eq!(content(&harness), Content::Waiting);

    harness.click("second.txt");
    assert_eq!(shown_text(&harness), "second.txt");
    harness.run(pending);
    harness.send(Message::Preview(PreviewMessage::Settled(stale)));
    harness.send(Message::Preview(PreviewMessage::Loaded(
        stale,
        Ok(b"first.txt".to_vec()),
    )));
    assert_eq!(shown_text(&harness), "second.txt");
}

// Dropping files

#[test]
fn files_dropped_onto_a_remote_tab_are_uploaded() {
    let mut harness = connected("drop-remote", |_| {});
    let paths = outside_files(&harness, &["one.txt", "two.txt", "folder/inner.txt"]);
    let paths = [
        paths[0].clone(),
        paths[1].clone(),
        paths[2].parent().map(Path::to_path_buf).unwrap_or_default(),
    ];
    let first = paths[0].clone();
    harness.send(Message::Window(
        harness.window,
        window::Event::FileHovered(first),
    ));
    assert_eq!(harness.app.file_drop_target(), Some(DropTarget::Pane(0)));
    assert!(harness.contains("Release to copy the files into demo@files.example.test"));
    harness.send(Message::Window(
        harness.window,
        window::Event::FilesHoveredLeft,
    ));
    assert_eq!(harness.app.file_drop_target(), None);

    drop_files(&mut harness, &paths);
    assert_eq!(harness.app.file_drop_target(), None);
    assert_eq!(harness.app.transfers.snapshots().len(), 3);
    assert!(harness.app.transfer_panel_open);
    harness.drive_transfers_until(finished);
    let remote = harness.root.remote();
    assert_eq!(
        fs::read_to_string(remote.join("one.txt")).expect("one.txt is missing"),
        "one.txt"
    );
    assert!(remote.join("two.txt").is_file());
    assert!(remote.join("folder").join("inner.txt").is_file());
    let names = harness.names();
    assert!(names.contains(&"one.txt".to_string()), "{names:?}");
}

#[test]
fn files_dropped_onto_a_local_tab_are_copied_there() {
    let mut harness = Harness::start("drop-local", Layout::Classic, |root| {
        write_files(&root.data(), &["target/"]);
    });
    let target = text_of(&harness.root.data().join("target"));
    let right = harness.app.panes[1].tabs[0].id;
    let input = harness.input_bounds(&crate::ui::path_input_id(right));
    harness.fill_at(input.center(), &target);
    harness.key(Named::Enter);
    let left = harness.bounds(&folder("target"));
    harness.move_to(left.center());
    harness.move_to(Point::new(VIEWPORT.width - 100.0, VIEWPORT.height / 2.0));
    assert_eq!(harness.app.pointer, Some(DropTarget::Pane(1)));

    let paths = outside_files(&harness, &["copy.txt"]);
    drop_files(&mut harness, &paths);
    harness.drive_transfers_until(finished);
    assert!(
        harness
            .root
            .data()
            .join("target")
            .join("copy.txt")
            .is_file()
    );
    assert!(!harness.root.data().join("copy.txt").exists());

    let again = harness.root.data().join("target").join("copy.txt");
    drop_files(&mut harness, &[again]);
    assert!(harness.has("Source and target are the same folder."));
}

// Copying paths

#[test]
fn ctrl_shift_c_copies_local_paths_and_server_urls() {
    let mut harness = local("copy-path");
    harness.click("notes.txt");
    harness.shortcut("c", Modifiers::CTRL | Modifiers::SHIFT);
    let expected = text_of(&harness.root.data().join("notes.txt"));
    assert_eq!(harness.clipboard, [expected]);
    assert!(harness.has("Path copied to the clipboard."));

    let mut harness = connected("copy-url", |root| {
        write_files(&root.remote(), &["a.txt", "b c.txt"]);
    });
    harness.shortcut("a", Modifiers::CTRL);
    harness.shortcut("c", Modifiers::CTRL | Modifiers::SHIFT);
    let remote = harness.root.remote();
    let expected = [
        fakes::server_url(SERVER, &text_of(&remote.join("a.txt"))),
        fakes::server_url(SERVER, &text_of(&remote.join("b c.txt"))),
    ]
    .join(LINE_BREAK);
    assert_eq!(harness.clipboard, std::slice::from_ref(&expected));
    assert!(expected.starts_with("sftp://demo@files.example.test/"));
    assert!(expected.contains("b%20c.txt"));
    assert!(!expected.contains(fakes::password()));
    assert!(harness.has("2 paths copied to the clipboard."));
}

// Layout

const REPORT: &str = "quarterly-report-final.txt";
const HEADERS: [&str; 4] = ["Name", "Type", "Size", "Modified"];
const HEADER_LINE: f32 = 13.0 * LINE_HEIGHT + 0.5;

fn report_folder(name: &str, layout: Layout, size: Size) -> Harness {
    let mut harness = Harness::start(name, layout, |root| {
        fs::write(root.data().join(REPORT), NOTES).expect("writing the report failed");
        write_files(&root.data(), &["photo.png", "docs/"]);
    });
    harness.resize(size);
    harness.key(Named::F3);
    harness.click(REPORT);
    assert_eq!(shown_text(&harness), NOTES);
    harness
}

fn assert_list_fits(harness: &mut Harness, case: &str) {
    let preview = harness.bounds("Preview");
    let mut labels: Vec<String> = harness
        .texts()
        .into_iter()
        .filter(|text| HEADERS.iter().any(|header| text.starts_with(header)))
        .collect();
    labels.sort();
    labels.dedup();
    let mut names = 0;
    for label in &labels {
        for (bounds, visible) in harness.find_texts(label) {
            if bounds.x >= preview.x {
                continue;
            }
            let visible = visible.unwrap_or_else(|| panic!("{case}: {label:?} is hidden"));
            assert!(
                visible.width + 0.5 >= bounds.width,
                "{case}: {label:?} is cut off: {visible:?} of {bounds:?}"
            );
            assert!(bounds.height <= HEADER_LINE, "{case}: {label:?} wraps");
            assert!(
                bounds.x + bounds.width <= preview.x,
                "{case}: {label:?} reaches into the preview"
            );
            names += usize::from(label.starts_with("Name"));
        }
    }
    assert_eq!(names, 1, "{case}: the Name header is missing");
    let (file, visible) = harness
        .find_texts(REPORT)
        .into_iter()
        .min_by(|first, second| first.0.x.total_cmp(&second.0.x))
        .unwrap_or_else(|| panic!("{case}: {REPORT} is not listed"));
    let visible = visible.unwrap_or_else(|| panic!("{case}: {REPORT} is hidden"));
    assert!(
        file.x < preview.x,
        "{case}: {REPORT} is only in the preview"
    );
    assert!(
        visible.width + 0.5 >= file.width,
        "{case}: the name column cuts {REPORT}: {visible:?} of {file:?}"
    );
    assert!(
        visible.x + visible.width <= preview.x,
        "{case}: {REPORT} overlaps"
    );
}

#[test]
fn the_file_list_keeps_its_name_column_next_to_the_preview() {
    let cases = [
        ("classic-large", Layout::Classic, Size::new(1500.0, 950.0)),
        ("classic-small", Layout::Classic, Size::new(1000.0, 700.0)),
        ("modern-small", Layout::Modern, Size::new(1000.0, 700.0)),
        ("modern-large", Layout::Modern, Size::new(1500.0, 950.0)),
    ];
    for (case, layout, size) in cases {
        let mut harness = report_folder(&format!("preview-fit-{case}"), layout, size);
        assert_list_fits(&mut harness, case);
        harness.app.preview.width = crate::preview::MAX_WIDTH;
        assert_list_fits(&mut harness, case);
    }
}

#[test]
fn dragging_the_divider_stops_before_the_file_list() {
    let mut harness = report_folder(
        "preview-drag-limit",
        Layout::Classic,
        Size::new(1500.0, 950.0),
    );
    let preview = harness.bounds("Preview");
    let divider = Point::new(preview.x - 9.0, preview.center_y() + 200.0);
    harness.move_to(divider);
    harness.press();
    assert!(harness.app.preview.is_resizing());
    harness.send(Message::Preview(PreviewMessage::Resized(divider.x)));
    harness.send(Message::Preview(PreviewMessage::Resized(0.0)));
    harness.send(Message::Preview(PreviewMessage::ResizeEnded));
    let width = harness.app.preview.width;
    assert!(width > crate::preview::MIN_WIDTH, "{width}");
    assert!(width < crate::preview::DEFAULT_WIDTH, "{width}");
    assert_list_fits(&mut harness, "dragged");
}
