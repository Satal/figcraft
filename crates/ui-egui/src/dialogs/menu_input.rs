use super::{Context, FigApp, Tokens, i18n, json, tr, window};

/// Values collected by a menu or Actions command before dispatching it to the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuInput {
    Link(String),
    Offset(f64),
    MoveToPage,
    /// The text of a Mermaid flowchart to import onto the board.
    Mermaid(String),
}

/// Menus and Actions cannot pass a value themselves. Gather it here, then run the regular
/// command so the same validation and undo behaviour apply as for programmatic callers.
pub(super) fn menu_input_dialog(app: &mut FigApp, ctx: &Context) {
    let Some(mut input) = app.ui.menu_input.take() else { return };
    let t = Tokens::get(ctx);
    let title = match &input {
        MenuInput::Link(_) => i18n::command("text.link", "Create link"),
        MenuInput::Offset(_) => i18n::command("vector.offset", "Offset path"),
        MenuInput::MoveToPage => i18n::command("object.moveToPage", "Move to page"),
        MenuInput::Mermaid(_) => i18n::command("board.importMermaid", "Import Mermaid flowchart"),
    };
    let mut open = true;
    let mut action: Option<(&str, serde_json::Value)> = None;
    let mut mermaid: Option<String> = None;
    window(&title, ctx).open(&mut open).show(ctx, |ui| {
        ui.set_width(if matches!(input, MenuInput::Mermaid(_)) { 480.0 } else { 320.0 });
        match &mut input {
            MenuInput::Link(url) => {
                ui.horizontal(|ui| {
                    let field = crate::widgets::text_input(ui, egui::Id::unique("menu_input_link"), url, 250.0, "https://");
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ok = !url.trim().is_empty();
                    if (crate::widgets::button(ui, tr("Done"), true, ok).clicked() || enter) && ok {
                        action = Some(("text.link", json!({"url": url.trim()})));
                    }
                });
            }
            MenuInput::Offset(offset) => {
                ui.horizontal(|ui| {
                    let id = egui::Id::unique("menu_input_offset");
                    if let Some(crate::widgets::FieldEdit::Set(v) | crate::widgets::FieldEdit::Scrub(v, _)) =
                        crate::widgets::number_field(ui, id, crate::widgets::Lead::None, Some(*offset), "", 120.0, 1.0)
                    {
                        *offset = v.clamp(-100_000.0, 100_000.0);
                    }
                    if crate::widgets::button(ui, tr("Offset"), true, true).clicked() {
                        action = Some(("vector.offset", json!({"offset": *offset})));
                    }
                });
            }
            MenuInput::Mermaid(text) => {
                let label = ui.label(
                    egui::RichText::new(tr("Paste or type a flowchart that starts with flowchart or graph"))
                        .font(crate::theme::regular(11.0))
                        .color(t.text_secondary),
                );
                let fid = egui::Id::unique("menu_input_mermaid");
                // Taken before the field sees it, so it imports instead of adding a line.
                let submit = ui.memory(|m| m.has_focus(fid)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
                let field = egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .id(fid)
                                // Not `code_editor`: it keeps Tab, a keyboard trap.
                                .font(egui::TextStyle::Monospace)
                                .hint_text("flowchart LR\n  A --> B")
                                .desired_rows(10)
                                .desired_width(f32::INFINITY),
                        )
                    })
                    .inner
                    .labelled_by(label.id);
                // Open with the cursor in the field; Tab leaves it for the button.
                if ui.memory(|m| m.focused().is_none()) {
                    field.request_focus();
                }
                let ok = !text.trim().is_empty();
                if (crate::widgets::button(ui, tr("Import"), true, ok).clicked() || submit) && ok {
                    mermaid = Some(text.clone());
                }
            }
            MenuInput::MoveToPage => {
                let mut choices = 0;
                if let Ok(ds) = app.session.ds() {
                    for &page in &ds.doc.pages {
                        if page == ds.page || ds.doc.is_divider(page) {
                            continue;
                        }
                        if let Some(node) = ds.doc.node(page)
                            && crate::widgets::button(ui, &node.name, false, true).clicked()
                        {
                            action = Some(("object.moveToPage", json!({"page": page})));
                        }
                        choices += 1;
                    }
                }
                if choices == 0 {
                    crate::widgets::label(ui, tr("No other pages"), t.text_secondary);
                }
            }
        }
    });
    if let Some(text) = mermaid {
        // A refused flowchart keeps the dialog open with the text, to fix it.
        if !import_mermaid(app, &text, None) {
            app.ui.menu_input = Some(MenuInput::Mermaid(text));
        }
    } else if let Some((id, params)) = action {
        app.run(id, params);
    } else if open {
        app.ui.menu_input = Some(input);
    }
}

/// Import Mermaid flowchart `text` onto the board, centred in the view or with its top-left at
/// `at`, and say what was drawn differently or left out. Whether it was imported.
pub(crate) fn import_mermaid(app: &mut FigApp, text: &str, at: Option<(f64, f64)>) -> bool {
    // The engine refuses an oversized chart in its own words; the user is told through the
    // translation system.
    if let Some((nodes, links)) = figcraft_engine::board::import_overflow(text) {
        app.show_error(i18n::trf(
            "The flowchart has {nodes} nodes and {links} links; at most {max_nodes} nodes and {max_links} links can be imported at once: nothing was added",
            &[
                ("nodes", &nodes),
                ("links", &links),
                ("max_nodes", &figcraft_engine::board::MAX_NODES),
                ("max_links", &figcraft_engine::board::MAX_EDGES),
            ],
        ));
        return false;
    }
    let mut params = json!({"text": text});
    if let Some((x, y)) = at {
        (params["x"], params["y"]) = (json!(x), json!(y));
    }
    let Some(out) = app.run("board.importMermaid", params) else { return false };
    let warnings: Vec<&str> = out["warnings"].as_array().map(|w| w.iter().filter_map(serde_json::Value::as_str).collect()).unwrap_or_default();
    if let Some(first) = warnings.first() {
        app.toast(i18n::trn(
            "Flowchart imported; {n} things were drawn differently or left out, such as {first}",
            warnings.len(),
            &[("first", first)],
        ));
    }
    true
}
