//! The toolbar's shape libraries popover on boards: the built-in libraries (flowchart, network,
//! UML, arrows) and the VSSX stencils loaded, each shape a button with its picture that adds it in
//! the middle of the view (`board.insertLibraryShape`), and a button to load another stencil.

use std::collections::HashMap;

use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use figcraft_engine::board::{LibraryShape, ShapeLibrary};
use figcraft_geom::{Affine, BezPath, Shape as _};
use serde_json::json;

use crate::FigApp;
use crate::a11y;
use crate::i18n::{tr, trf, trn};
use crate::icons::{self, Icon};
use crate::theme::{Tokens, regular, semibold};
use crate::toolbar::tool_cell;

/// The popover's key while it is open (`app.ui.tool_flyout`).
const KEY: &str = "shape_libraries";
const CELL: f32 = 44.0;
const COLUMNS: usize = 7;
const PAD: f32 = 10.0;
const GAP: f32 = 4.0;
const HEIGHT: f32 = 420.0;
/// The largest stencil icon drawn.
const MAX_ICON: u32 = 256;

/// The shape libraries button at `at` (its left edge, the toolbar's middle) and, while open, its
/// popover above the toolbar (whose top is `top`). The button's right edge.
pub(crate) fn button(app: &mut FigApp, ui: &mut Ui, at: Pos2, top: f32, t: &Tokens) -> f32 {
    let r = Rect::from_min_size(pos2(at.x, at.y - 16.0), vec2(32.0, 32.0));
    let open = app.ui.tool_flyout == Some(KEY);
    let resp = tool_cell(ui, t, r, "shape_libraries", Icon::Library, open, tr("Shape libraries"));
    if open {
        a11y::popup(&mut app.ui.nav, resp.id, area_id());
    }
    if resp.clicked() {
        if !open {
            // Reads the stencils kept in preferences again, if they aren't yet.
            app.run("board.shapeLibraries", json!({}));
        }
        app.ui.tool_flyout = if open { None } else { Some(KEY) };
    }
    if app.ui.tool_flyout == Some(KEY) {
        popover(app, ui, r, top - 8.0, t);
    }
    r.max.x
}

fn area_id() -> egui::Id {
    egui::Id::unique("shape_libraries_area")
}

/// A built-in library's or shape's name, translated; a stencil's as it is.
fn name_of<'a>(lib: &ShapeLibrary, text: &'a str) -> &'a str {
    if lib.built_in { tr(text) } else { text }
}

/// Stencil icons, decoded the first time they are shown (`None`: it couldn't be).
#[derive(Clone, Default)]
struct Icons(HashMap<(String, String), Option<egui::TextureHandle>>);

fn icon_texture(ctx: &egui::Context, lib: &ShapeLibrary, sh: &LibraryShape) -> Option<egui::TextureHandle> {
    let png = sh.icon_png.as_ref()?;
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    if img.width() == 0 || img.height() == 0 || img.width() > MAX_ICON || img.height() > MAX_ICON {
        return None;
    }
    let rgba = img.to_rgba8();
    let size = [usize::try_from(rgba.width()).ok()?, usize::try_from(rgba.height()).ok()?];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture(format!("shape-library-{}-{}", lib.id, sh.name), image, egui::TextureOptions::LINEAR))
}

/// Shape `sh`'s outline fitted in a 100-unit square.
fn fitted(sh: &LibraryShape) -> BezPath {
    let b = sh.path.bounding_box();
    let side = b.width().max(b.height());
    if !(side.is_finite() && side > 0.0) {
        return BezPath::new();
    }
    let k = 84.0 / side;
    let c = b.center();
    Affine::translate((50.0, 50.0)) * Affine::scale(k) * Affine::translate((-c.x, -c.y)) * sh.path.clone()
}

/// What the popover was asked to do.
enum Choice {
    Insert(String, String),
    Remove(String),
    Load,
}

fn popover(app: &mut FigApp, ui: &mut Ui, anchor: Rect, bottom: f32, t: &Tokens) {
    let ctx = ui.ctx().clone();
    let libs: Vec<ShapeLibrary> = figcraft_engine::board::built_in().iter().chain(&app.session.shape_libraries).cloned().collect();
    let mut cache: Icons = ctx.data(|d| d.get_temp(area_id())).unwrap_or_default();
    let width = COLUMNS as f32 * (CELL + GAP) - GAP + 2.0 * PAD;
    let canvas = app.canvas.rect;
    let height = HEIGHT.min((bottom - canvas.min.y - 8.0).max(160.0));
    let x = (anchor.max.x - width).min(canvas.max.x - width - 8.0).max(canvas.min.x + 8.0);
    let mut choice = None;
    egui::Area::new(area_id()).fixed_pos(pos2(x, bottom - height)).order(egui::Order::Foreground).show(&ctx, |ui| {
        let rect = Rect::from_min_size(ui.min_rect().min, vec2(width, height));
        ui.painter().rect_filled(rect, CornerRadius::same(13), t.menu);
        a11y::container_bounds(ui, rect);
        let inner = rect.shrink(PAD);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.label(egui::RichText::new(tr("Shape libraries")).font(semibold(12.0)).color(t.menu_text));
        egui::ScrollArea::vertical().max_height(inner.height() - 22.0).auto_shrink([false, false]).show(&mut child, |ui| {
            for lib in &libs {
                library(ui, &ctx, lib, &mut cache, t, &mut choice);
            }
            ui.add_space(6.0);
            let label = tr("Load VSSX stencil…");
            let (r, resp) = ui.allocate_exact_size(vec2(inner.width() - 12.0, 26.0), Sense::click());
            a11y::menu_item(&resp, label, "", None);
            a11y::own_focus(&resp);
            if resp.hovered() || resp.has_focus() {
                ui.painter().rect_filled(r, CornerRadius::same(6), t.menu_hover);
            }
            ui.painter().text(pos2(r.min.x + 6.0, r.center().y), Align2::LEFT_CENTER, label, regular(12.0), t.menu_text);
            if resp.clicked() {
                choice = Some(Choice::Load);
            }
        });
        ui.allocate_rect(rect, Sense::hover());
    });
    ctx.data_mut(|d| d.insert_temp(area_id(), cache));
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.tool_flyout = None;
    }
    match choice {
        Some(Choice::Insert(library, name)) => {
            app.run("board.insertLibraryShape", json!({"library": library, "name": name}));
            app.ui.tool_flyout = None;
        }
        Some(Choice::Remove(library)) => remove_library(app, &library),
        Some(Choice::Load) => load_stencil(app),
        None => {}
    }
}

/// One library: its name, why it couldn't be read, its shapes and (a stencil) a remove button.
fn library(ui: &mut Ui, ctx: &egui::Context, lib: &ShapeLibrary, cache: &mut Icons, t: &Tokens, choice: &mut Option<Choice>) {
    ui.add_space(6.0);
    let title = name_of(lib, &lib.name).to_string();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&title).font(semibold(11.0)).color(t.menu_text));
        if !lib.built_in {
            let label = trf("Remove {name}", &[("name", &title)]);
            let resp = ui.add(egui::Button::new(egui::RichText::new(tr("Remove")).font(regular(11.0)).color(t.menu_text)).frame(false));
            a11y::named_button(&resp, &label);
            if resp.clicked() {
                *choice = Some(Choice::Remove(lib.id.clone()));
            }
        }
    });
    if let Some(e) = &lib.error {
        ui.label(egui::RichText::new(trf("Couldn't read it: {error}", &[("error", e)])).font(regular(11.0)).color(t.menu_text));
        return;
    }
    for row in lib.shapes.chunks(COLUMNS) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for sh in row {
                let label = name_of(lib, &sh.label).to_string();
                let (r, resp) = ui.allocate_exact_size(vec2(CELL, CELL), Sense::click());
                let resp = resp.on_hover_text(&label);
                a11y::menu_item(&resp, &trf("{shape}, {library}", &[("shape", &label), ("library", &title)]), "", None);
                a11y::own_focus(&resp);
                let p = ui.painter();
                if resp.hovered() || resp.has_focus() {
                    p.rect_filled(r, CornerRadius::same(6), t.menu_hover);
                }
                p.rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, t.border), StrokeKind::Inside);
                let key = (lib.id.clone(), sh.name.clone());
                let tex = cache.0.entry(key).or_insert_with(|| icon_texture(ctx, lib, sh)).clone();
                let pic = r.shrink(7.0);
                match tex {
                    Some(tex) => {
                        let s = tex.size_vec2();
                        let k = (pic.width() / s.x).min(pic.height() / s.y);
                        p.image(
                            tex.id(),
                            Rect::from_center_size(pic.center(), s * k),
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    None => icons::paint_outlines(p, pic, 100.0, &[(&fitted(sh), false)], t.menu_text, 5.0),
                }
                if resp.clicked() {
                    *choice = Some(Choice::Insert(lib.id.clone(), sh.name.clone()));
                }
            }
        });
    }
}

/// Forgets loaded stencil `library`.
fn remove_library(app: &mut FigApp, library: &str) {
    if app.run("board.removeShapeLibrary", json!({"library": library})).is_some() {
        app.toast(tr("Shape library removed"));
    }
}

/// Picks a VSSX stencil and loads it as a shape library.
fn load_stencil(app: &mut FigApp) {
    app.pick_file(tr("Load VSSX stencil"), &["vssx"], true, |app, file| {
        if let Some(r) = app.run_user("board.loadShapeLibrary", file) {
            let name = r["name"].as_str().unwrap_or_default().to_string();
            let n = r["shapes"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
            let warned = r["warnings"].as_array().map_or(0, Vec::len);
            let msg = trn("Loaded shape library {name}: {n} shapes", n, &[("name", &name)]);
            app.toast(if warned > 0 { format!("{msg} ({})", tr("some shapes couldn't be read exactly")) } else { msg });
        }
    });
}
