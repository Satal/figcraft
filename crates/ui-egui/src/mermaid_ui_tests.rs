//! Mermaid flowcharts on a board from the UI: File ▸ Import Mermaid flowchart… opens a dialog that
//! takes the text by keyboard; pasting a flowchart on a board imports it; elsewhere it stays text.

use egui::{Event, Key, Modifiers};
use figcraft_doc::{BoardItem, Document, NodeKind};
use figcraft_engine::Session;
use serde_json::json;

use crate::dialogs::MenuInput;
use crate::layers_drag_tests::Ui;
use crate::{FigApp, NoServices, menus};

const CHART: &str = "flowchart LR\n  a[Plan] --> b[Build] --> c[Ship]\n";

fn shapes(doc: &Document) -> usize {
    doc.ids().filter(|i| doc.node(*i).is_some_and(|n| matches!(n.board, Some(BoardItem::Shape { .. })))).count()
}

fn connectors(doc: &Document) -> usize {
    doc.ids().filter(|i| doc.node(*i).is_some_and(|n| matches!(n.board, Some(BoardItem::Connector(_))))).count()
}

fn session(board: bool) -> Session {
    let mut s = Session::new();
    if board {
        s.execute("file.newBoard", &json!({})).unwrap();
    }
    s
}

/// Open the dialog, type `text` and press Ctrl+Enter.
fn import_by_keyboard(u: &mut Ui, text: &str) {
    menus::invoke(&mut u.app, "board.importMermaid");
    assert_eq!(u.app.ui.menu_input, Some(MenuInput::Mermaid(String::new())));
    u.frame(vec![]);
    u.frame(vec![]);
    // The field has the keyboard as the dialog opens.
    u.frame(vec![Event::Text(text.into())]);
    let ctrl = Modifiers::COMMAND;
    u.frame(vec![
        Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: ctrl },
        Event::Key { key: Key::Enter, physical_key: None, pressed: false, repeat: false, modifiers: ctrl },
    ]);
}

#[test]
fn the_import_dialog_takes_a_flowchart_by_keyboard() {
    let mut u = Ui::new(session(true));
    import_by_keyboard(&mut u, CHART.trim_end());
    let doc = u.app.session.doc().unwrap();
    assert_eq!((shapes(doc), connectors(doc)), (3, 2), "the flowchart is on the board");
    assert!(u.app.ui.menu_input.is_none(), "the dialog closed");
    assert!(u.app.ui.last_error.is_none());
}

#[test]
fn a_refused_flowchart_keeps_the_dialog_and_its_text() {
    let mut u = Ui::new(session(true));
    import_by_keyboard(&mut u, "sequenceDiagram");
    assert!(u.app.ui.last_error.is_some(), "it says why nothing was added");
    assert!(matches!(&u.app.ui.menu_input, Some(MenuInput::Mermaid(t)) if t.starts_with("sequenceDiagram")), "{:?}", u.app.ui.menu_input);
    assert_eq!(shapes(u.app.session.doc().unwrap()), 0);
}

#[test]
fn on_a_design_file_the_menu_entry_says_it_is_for_boards() {
    let mut u = Ui::new(session(false));
    menus::invoke(&mut u.app, "board.importMermaid");
    assert!(u.app.ui.menu_input.is_none(), "no dialog to fill in for nothing");
    assert!(u.app.ui.last_error.is_some());
}

#[test]
fn pasting_a_flowchart_on_a_board_imports_it_and_elsewhere_it_stays_text() {
    let mut app = FigApp::new(session(true), Box::new(NoServices));
    menus::paste(&mut app, Some(CHART), None);
    let doc = app.session.doc().unwrap();
    assert_eq!((shapes(doc), connectors(doc)), (3, 2));
    assert!(app.ui.toast.is_none(), "drawn as written: nothing to say");
    // A shape without an outline of its own is imported and said.
    menus::paste(&mut app, Some("graph TD\n s@{ shape: cloud, label: \"Sky\" } --> t"), Some((2000.0, 0.0)));
    assert!(app.ui.toast.as_ref().is_some_and(|(m, _)| m.contains("drawn differently")), "{:?}", app.ui.toast);
    // In a design file the same text is a text layer.
    let mut design = FigApp::new(session(false), Box::new(NoServices));
    let texts = |d: &Document| d.ids().filter(|i| d.node(*i).is_some_and(|n| matches!(n.kind, NodeKind::Text(_)))).count();
    let before = texts(design.session.doc().unwrap());
    menus::paste(&mut design, Some(CHART), None);
    assert_eq!(texts(design.session.doc().unwrap()), before + 1);
}

#[test]
fn tab_leaves_the_flowchart_field() {
    let mut u = Ui::new(session(true));
    menus::invoke(&mut u.app, "board.importMermaid");
    u.frame(vec![]);
    u.frame(vec![]);
    let field = egui::Id::unique("menu_input_mermaid");
    assert!(u.ctx.memory(|m| m.has_focus(field)), "the field has the keyboard");
    u.frame(vec![Event::Text("graph TD".into())]);
    u.frame(vec![Event::Key { key: Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
    u.frame(vec![]);
    assert!(!u.ctx.memory(|m| m.has_focus(field)), "Tab moves on, no trap");
    assert!(matches!(&u.app.ui.menu_input, Some(MenuInput::Mermaid(t)) if !t.contains('\t')), "{:?}", u.app.ui.menu_input);
}
