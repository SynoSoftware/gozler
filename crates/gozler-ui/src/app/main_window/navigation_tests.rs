#![cfg(test)]

use super::*;
use crate::core::fetch::{Chunk, Fetched, Outcome};
use crate::core::settings::Settings;
use crate::core::startup::Search as StartupSearch;
use crate::core::text::tests::english;
use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_HOME};

#[test]
fn show_in_commander_gives_the_destination_panel_keyboard_focus() {
    win::shell::initialize_com();
    win::shell::initialize_ole();
    win::controls::initialize();
    let class = "gozler.test.show-in-commander-focus";
    win::window::register_class::<MainWindow>(
        class,
        crate::core::ids::IconId::Gozler.id(),
        win::window::Background::Window,
    )
    .expect("the test's window class registers");
    win::pane::register().expect("the pane class registers once in this test process");

    let fixture =
        std::env::temp_dir().join(format!("gozler-navigation-focus-{}", std::process::id()));
    std::fs::create_dir_all(fixture.join("child")).expect("the fixture directory is writable");
    let selected = fixture.join("selected.txt");
    std::fs::write(&selected, b"fixture").expect("the fixture file is writable");
    let mut settings = Settings::default();
    settings.window.panels.left.folder = Some(fixture.join("child").to_string_lossy().into_owned());
    settings.window.panels.right.folder = settings.window.panels.left.folder.clone();
    let mut model = Model::new(english(), settings, Instance::Another, false);
    model.set_typed_search(List::Search, "navigation-focus-fixture".into());
    let settings_file = std::env::temp_dir().join(format!(
        "gozler-navigation-focus-{}.json",
        std::process::id()
    ));
    let (hwnd, window) = MainWindow::open(class, model, settings_file, None, 0, false, None);
    struct Opened(Hwnd, Rc<MainWindow>, PathBuf);
    impl Drop for Opened {
        fn drop(&mut self) {
            self.0.destroy();
            self.1.finish_shutdown();
            std::fs::remove_file(self.2.join("selected.txt")).expect("the fixture file is ours");
            std::fs::remove_dir(self.2.join("child")).expect("the child fixture is empty");
            std::fs::remove_dir(&self.2).expect("the root fixture is empty");
        }
    }
    let _opened = Opened(hwnd, Rc::clone(&window), fixture.clone());
    let generation = window
        .created()
        .work
        .borrow()
        .requests
        .current(Purpose::Results(List::Search))
        .expect("opening the window starts its search");
    window
        .created()
        .work
        .borrow_mut()
        .requests
        .cancel(Purpose::Results(List::Search));
    let accepted = window.model.borrow_mut().accept(Fetched {
        purpose: Purpose::Results(List::Search),
        generation,
        outcome: Outcome::Rows(Chunk {
            start: 0,
            rows: vec![ResultItem {
                path: selected.to_string_lossy().into_owned(),
                path_u16: None,
                size: None,
                mtime_ft: None,
                created_ft: None,
                is_dir: false,
                attributes: None,
            }]
            .into(),
            total: 1,
            volumes_incomplete: false,
            metadata_pending: false,
            sums: None,
            rest_follows: false,
            made: None,
        }),
    });
    window.apply(hwnd, accepted);
    let search = window.controls().search.row.input.hwnd();
    search.set_text("navigation-focus-fixture");
    search.focus();
    assert_eq!(Hwnd::focused(), Some(search));
    hwnd.set_timer(0x7fff, 10);
    let until = |done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(3);
        win::window::run_messages_until(hwnd, &|| window.accelerators(), &|| {
            done() || Instant::now() >= deadline
        });
        assert!(
            done(),
            "the native navigation state must finish within the test budget"
        );
    };
    window.controls().search.list.select_only(Some(0));
    window.run(hwnd, MenuItem::Run(CommandId::ShowInCommander));
    until(&|| {
        if window.model.borrow().mode() == Mode::Search {
            assert!(!window.panel(Side::Left).list.hwnd().is_visible());
            assert!(!window.panel(Side::Right).list.hwnd().is_visible());
            return false;
        }
        for side in Side::BOTH {
            assert!(
                !matches!(
                    window.model.borrow().panels().panel(side).view(),
                    panel::View::Reading
                ),
                "Commander must first appear with both listings ready"
            );
        }
        assert_eq!(window.panel(Side::Left).list.count(), 3);
        assert_eq!(window.panel(Side::Right).list.count(), 1);
        assert_eq!(Hwnd::focused(), Some(window.home()));
        true
    });
    assert!(window.model.borrow().mode() == Mode::Commander);
    let side = window.model.borrow().panels().active();
    let list = window.panel(side).list;
    until(&|| list.count() == 3 && list.focused().is_some());
    win::window::post_key(list.hwnd(), VK_HOME);
    assert_eq!(
        Hwnd::focused(),
        Some(window.home()),
        "Show in Commander must give the destination panel its keyboard focus"
    );
    until(&|| list.focused() == Some(0));
    win::window::post_key(list.hwnd(), VK_DOWN);
    until(&|| list.focused() == Some(1));
    window.run(hwnd, MenuItem::Mode(Mode::Search));
    assert_eq!(window.model.borrow().mode(), Mode::Search);
    assert_eq!(Hwnd::focused(), Some(search));
    let (_, cancelled) = window.controls().panels.show_entry_in_commander(
        &window,
        Selected {
            path: fixture
                .join("child")
                .to_string_lossy()
                .encode_utf16()
                .collect(),
            is_folder: true,
        },
    );
    assert_eq!(window.controls().panels.entering_side(), Some(side));
    window.show_mode(hwnd, Mode::Search);
    assert!(window.controls().panels.entering_side().is_none());
    for side in Side::BOTH {
        assert!(!window.created().work.borrow().folders.watches(side));
        assert!(
            window
                .model
                .borrow()
                .panels()
                .panel(side)
                .reading_generation()
                .is_none()
        );
    }
    window.controls().panels.on_read(
        &window,
        hwnd,
        side,
        panel::ReadEvent::Listed {
            generation: cancelled,
            folder: fixture.join("child").to_string_lossy().into_owned(),
            entries: Vec::new(),
            focus: None,
        },
    );
    assert_eq!(window.model.borrow().mode(), Mode::Search);
    window.controls().search.list.hwnd().focus();
    window.run(hwnd, MenuItem::Mode(Mode::Search));
    assert_eq!(window.model.borrow().mode(), Mode::Search);
    assert_eq!(Hwnd::focused(), Some(window.controls().search.list.hwnd()));
    assert_eq!(
        window.controls().search.row.input.text(),
        "navigation-focus-fixture"
    );
    assert_eq!(window.controls().search.list.focused(), Some(0));
    window.run(hwnd, MenuItem::Mode(Mode::Commander));
    let path = window.panel(side).path.hwnd();
    path.focus();
    window.run(hwnd, MenuItem::Mode(Mode::Commander));
    assert_eq!(window.model.borrow().mode(), Mode::Commander);
    assert_eq!(Hwnd::focused(), Some(path));
    assert_eq!(list.focused(), Some(1));

    assert_eq!(
        window.message(
            hwnd,
            Message::CopyData {
                tag: window.show_message as usize,
                text: String::new(),
            },
        ),
        Some(LRESULT(0))
    );
    assert_eq!(window.model.borrow().mode(), Mode::Commander);
    assert!(window.modal_steps.borrow().is_empty());

    hwnd.set_enabled(false);
    let first = StartupRequest::Search(StartupSearch {
        text: "superseded request".into(),
        ..Default::default()
    });
    let latest = StartupRequest::Search(StartupSearch {
        text: "latest request".into(),
        location: Some(fixture.to_string_lossy().into_owned()),
        mode: Some(MatchMode::Anywhere),
        case: Some(true),
        path: Some(false),
        sort: Some(gozler_core::types::SortKey::Size),
        direction: Some(SortDir::Asc),
    });
    assert!(win::instance::hand_over(hwnd, 0, Some(&first)));
    assert!(win::instance::hand_over(hwnd, 0, Some(&latest)));
    window.message(hwnd, Message::Deferred);
    assert_eq!(
        window.controls().search.row.input.text(),
        "navigation-focus-fixture"
    );
    assert_eq!(
        window
            .modal_steps
            .borrow()
            .iter()
            .filter(|step| matches!(step, ModalStep::Request(_)))
            .count(),
        1
    );
    hwnd.set_enabled(true);
    until(&|| window.controls().search.row.input.text() == "latest request");
    assert_eq!(window.model.borrow().mode(), Mode::Search);
    assert_eq!(
        window.model.borrow().matching(),
        Matching {
            mode: MatchMode::Anywhere,
            case: true,
            path: false
        }
    );
    assert_eq!(
        window.model.borrow().location(),
        Some(fixture.to_string_lossy().as_ref())
    );

    let reveal = StartupRequest::Reveal(selected.to_string_lossy().into_owned());
    assert!(win::instance::hand_over(hwnd, 0, Some(&reveal)));
    until(&|| {
        let focused = list.focused();
        let model = window.model.borrow();
        model.mode() == Mode::Commander && focused.is_some_and(|row| {
            matches!(model.panels().row(side, row), Some(panel::Row::Entry(entry)) if entry.display_name() == "selected.txt")
        })
    });
    assert_eq!(Hwnd::focused(), Some(list.hwnd()));
    assert_eq!(
        list.selected_count(),
        0,
        "Reveal must move the cursor without marking the file"
    );
    assert!(list.visible_rows().contains(&list.focused().unwrap()));
    until(&|| !window.model.borrow().panels().panel(side).rows_pending());

    let searched_side = side.other();
    window
        .model
        .borrow_mut()
        .panels_mut()
        .enter_search(searched_side);
    window.controls().panels.show_panel(&window, searched_side);
    window
        .reveal
        .replace(Some(Reveal::Checking(window.model.borrow().navigation())));
    window.search(hwnd, List::Search);
    search.set_text("latest request");
    assert!(
        window.reveal.borrow().is_some(),
        "a background Search refresh must not cancel Reveal"
    );
    window
        .controls()
        .panels
        .on_panel_search_text(&window, hwnd, searched_side);
    window
        .controls()
        .panels
        .on_look_in(&window, hwnd, searched_side);
    assert!(
        window.reveal.borrow().is_some(),
        "unchanged search controls do not express newer navigation"
    );
    let panel_search = window.panel(searched_side).search.hwnd();
    panel_search.set_text("newer query");
    panel_search.set_text("");
    assert!(
        window.reveal.borrow().is_none(),
        "editing a panel supersedes preflight even when the text returns"
    );

    window
        .reveal
        .replace(Some(Reveal::Checking(window.model.borrow().navigation())));
    let filters = window.model.borrow().filters().clone();
    window
        .model
        .borrow_mut()
        .choose_folder(fixture.join("child").to_string_lossy().into_owned());
    window.model.borrow_mut().set_filters(filters);
    window.message(hwnd, Message::Deferred);
    assert!(
        window.reveal.borrow().is_none(),
        "changing Search's scope and restoring it supersedes Reveal"
    );

    let retained = window
        .model
        .borrow()
        .panels()
        .panel(side)
        .folder()
        .to_owned();
    for (calculated, edit_search) in [(false, false), (true, false), (false, true), (true, true)] {
        if edit_search {
            window
                .model
                .borrow_mut()
                .panels_mut()
                .enter_search(searched_side);
            window.controls().panels.show_panel(&window, searched_side);
        }
        let ask = window.model.borrow_mut().panels_mut().go(
            side,
            fixture.join("child").to_string_lossy().into_owned(),
            None,
        );
        window.reveal.replace(Some(Reveal::Reading {
            origin: window.model.borrow().navigation(),
            side,
            generation: ask.generation,
        }));
        let candidate = if calculated {
            let entries = (0..=crate::core::work::INLINE_ITEMS)
                .map(|index| {
                    panel::Entry::new(
                        format!("file-{index:06}.txt").encode_utf16().collect(),
                        0,
                        0,
                        0,
                    )
                })
                .collect();
            let accepted = window.model.borrow_mut().panels_mut().accept_read(
                side,
                panel::ReadEvent::Listed {
                    generation: ask.generation,
                    folder: ask.path.clone(),
                    entries,
                    focus: None,
                },
                None,
            );
            assert_eq!(accepted.change, panel::ReadChange::Pending);
            Some(
                window
                    .model
                    .borrow_mut()
                    .panels_mut()
                    .take_calculation(side)
                    .unwrap(),
            )
        } else {
            None
        };
        if edit_search {
            if calculated {
                panel_search.set_text("newer calculated query");
            } else {
                window.panel(searched_side).look_in.select(1);
                window
                    .controls()
                    .panels
                    .on_look_in(&window, hwnd, searched_side);
            }
        } else {
            window.controls().panels.show_folder(
                &window,
                searched_side,
                fixture.to_string_lossy().into_owned(),
            );
        }
        assert!(
            window.reveal.borrow().is_none(),
            "new navigation or search in either panel supersedes Reveal"
        );
        if let Some(candidate) = candidate {
            assert!(
                window
                    .model
                    .borrow_mut()
                    .panels_mut()
                    .accept_calculation(side, candidate.run())
                    .is_none()
            );
        } else {
            let accepted = window.model.borrow_mut().panels_mut().accept_read(
                side,
                panel::ReadEvent::Listed {
                    generation: ask.generation,
                    folder: ask.path,
                    entries: Vec::new(),
                    focus: None,
                },
                None,
            );
            assert_eq!(accepted.change, panel::ReadChange::Stale);
        }
        assert_eq!(
            window.model.borrow().panels().panel(side).folder(),
            retained
        );
    }
    hwnd.kill_timer(0x7fff);
}
