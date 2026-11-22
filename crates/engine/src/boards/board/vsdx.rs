//! VSDX drawings on boards: `board.exportVsdx` writes the current page as a drawing (shapes with
//! their outlines, text and colours, glued connectors, sections as titled background rectangles,
//! stickies as filled rectangles) and `board.importVsdx` adds a drawing's page to the board in one
//! undo step. What can't be carried across is listed in the result's `warnings`, never dropped
//! without a word.

use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use craft_vsdx as vx;
use figcraft_color::{Color, Paint};
use figcraft_doc::{BoardItem, BoardShape, ConnectorPath, Document, End, NodeId, NodeKind, Side, StrokeCap, VectorNetwork};
use figcraft_geom::kurbo::{Arc, Circle, PathEl, Shape as _, Vec2};
use figcraft_geom::{Affine, BezPath, Point, Rect};
use serde_json::{Value, json};

use super::derived::{place_route, route};
use super::edit::label_of;
use super::items::{shape_padding, text_of};
use super::{Result, Session, bad, id_of, num, require, string};
use crate::EngineError;

/// The largest drawing or stencil file read.
pub(super) const MAX_FILE: u64 = 256 << 20;
/// Room around what an exported page holds.
const MARGIN: f64 = 36.0;
/// The dashes a dashed imported line gets.
const DASHES: [f64; 2] = [6.0, 4.0];
/// Paths of more segments than this aren't matched against the board's outlines.
const MAX_MATCH_SEGMENTS: usize = 256;
/// An imported outline of more segments than this is drawn as its box.
const MAX_OUTLINE_SEGMENTS: usize = 20_000;
/// The most shapes and connectors one import adds; the rest are left out and counted.
const MAX_LAYERS: usize = 5_000;
/// How deep exported sections nest.
const MAX_DEPTH: usize = 32;
/// What exported sections and stickies are called in the drawing: the marker that brings them
/// back as sections and stickies. A foreign drawing's shape called "Section" is only a shape.
const SECTION_NAME: &str = "FigCraft Section";
const STICKY_NAME: &str = "FigCraft Sticky";
/// What an imported section with no text is called.
const SECTION_TITLE: &str = "Section";
/// A shape whose outline is this close (of its longer side) to a board outline is that outline
/// to the eye and in effect; one within [`NEAR`] only resembles it.
const SAME: f64 = 0.005;
const NEAR: f64 = 0.03;

// ---------------------------------------------------------------------------------------------
// Geometry both ways.

/// A drawing path in a box as a kurbo path (arcs and ellipses as cubics). `None`: it has a
/// number that isn't finite.
pub(super) fn geom_to_bez(paths: &[vx::GeomPath]) -> Option<BezPath> {
    let mut out = BezPath::new();
    for path in paths {
        let mut cur: Option<Point> = None;
        let mut start: Option<Point> = None;
        for seg in &path.segments {
            match *seg {
                vx::Segment::MoveTo(p) => {
                    out.move_to(pt(p));
                    cur = Some(pt(p));
                    start = cur;
                }
                vx::Segment::LineTo(p) => {
                    begin(&mut out, &mut cur, &mut start);
                    out.line_to(pt(p));
                    cur = Some(pt(p));
                }
                vx::Segment::CubicTo { c1, c2, to } => {
                    begin(&mut out, &mut cur, &mut start);
                    out.curve_to(pt(c1), pt(c2), pt(to));
                    cur = Some(pt(to));
                }
                vx::Segment::ArcTo { to, via, rotation, ratio } => {
                    let from = begin(&mut out, &mut cur, &mut start);
                    arc_through(&mut out, from, pt(via), pt(to), rotation, ratio);
                    cur = Some(pt(to));
                }
                vx::Segment::Ellipse { centre, a, b } => {
                    let (c, a, b) = (pt(centre), pt(a), pt(b));
                    let t = Affine::new([a.x - c.x, a.y - c.y, b.x - c.x, b.y - c.y, c.x, c.y]);
                    out.extend(t * Circle::new((0.0, 0.0), 1.0).to_path(1e-4));
                    cur = None;
                    start = None;
                }
            }
        }
        if path.closed && cur.is_some() {
            out.close_path();
        }
    }
    finite(&out).then_some(out)
}

fn pt(p: vx::Point) -> Point {
    Point::new(p.0, p.1)
}

/// The current point, starting a path at the origin when a segment comes first.
fn begin(out: &mut BezPath, cur: &mut Option<Point>, start: &mut Option<Point>) -> Point {
    match cur {
        Some(p) => *p,
        None => {
            let p = Point::ZERO;
            out.move_to(p);
            *cur = Some(p);
            *start = Some(p);
            p
        }
    }
}

/// An elliptical arc from `from` through `via` to `to`, its major axis turned `rotation` degrees
/// clockwise and `ratio` times its minor one; a line when the three points are in a line.
fn arc_through(out: &mut BezPath, from: Point, via: Point, to: Point, rotation: f64, ratio: f64) {
    let ratio = if ratio.is_finite() && ratio > 1e-6 { ratio } else { 1.0 };
    let rot = if rotation.is_finite() { rotation.to_radians() } else { 0.0 };
    // Into a space where the ellipse is a circle: unturned, the minor axis stretched.
    let to_circle = Affine::scale_non_uniform(1.0, ratio) * Affine::rotate(-rot);
    let back = to_circle.inverse();
    let (a, b, c) = (to_circle * from, to_circle * via, to_circle * to);
    let Some((centre, r)) = circle_through(a, b, c) else {
        out.line_to(to);
        return;
    };
    let ang = |p: Point| (p.y - centre.y).atan2(p.x - centre.x);
    let (t0, tv, t1) = (ang(a), ang(b), ang(c));
    let tau = std::f64::consts::TAU;
    let ccw = |x: f64| x.rem_euclid(tau);
    // The sweep from a to c that passes b.
    let (sv, s1) = (ccw(tv - t0), ccw(t1 - t0));
    let sweep = if sv <= s1 { s1 } else { s1 - tau };
    let arc = Arc::new(centre, Vec2::new(r, r), t0, sweep, 0.0);
    arc.to_cubic_beziers(0.1, |p1, p2, p| out.curve_to(back * p1, back * p2, back * p));
}

/// The circle through three points (`None`: in a line).
fn circle_through(a: Point, b: Point, c: Point) -> Option<(Point, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    let scale = (a - c).hypot().max((b - c).hypot()).max(1e-9);
    if d.abs() < 1e-9 * scale * scale {
        return None;
    }
    let sq = |p: Point| p.x * p.x + p.y * p.y;
    let ux = (sq(a) * (b.y - c.y) + sq(b) * (c.y - a.y) + sq(c) * (a.y - b.y)) / d;
    let uy = (sq(a) * (c.x - b.x) + sq(b) * (a.x - c.x) + sq(c) * (b.x - a.x)) / d;
    let centre = Point::new(ux, uy);
    let r = (a - centre).hypot();
    (r.is_finite() && centre.x.is_finite() && centre.y.is_finite()).then_some((centre, r))
}

fn finite(p: &BezPath) -> bool {
    p.elements().iter().all(|e| match e {
        PathEl::MoveTo(a) | PathEl::LineTo(a) => a.is_finite(),
        PathEl::QuadTo(a, b) => a.is_finite() && b.is_finite(),
        PathEl::CurveTo(a, b, c) => a.is_finite() && b.is_finite() && c.is_finite(),
        PathEl::ClosePath => true,
    })
}

/// A kurbo path as drawing paths: one per subpath, an open one neither filled nor closed.
pub(super) fn bez_to_geom(path: &BezPath) -> Vec<vx::GeomPath> {
    let mut out: Vec<vx::GeomPath> = Vec::new();
    let mut cur = Point::ZERO;
    let p = |q: Point| (q.x, q.y);
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(a) => {
                out.push(vx::GeomPath { segments: vec![vx::Segment::MoveTo(p(a))], ..Default::default() });
                cur = a;
            }
            PathEl::LineTo(a) => {
                push_seg(&mut out, cur, vx::Segment::LineTo(p(a)));
                cur = a;
            }
            PathEl::QuadTo(q, a) => {
                let (c1, c2) = (cur + (q - cur) * (2.0 / 3.0), a + (q - a) * (2.0 / 3.0));
                push_seg(&mut out, cur, vx::Segment::CubicTo { c1: p(c1), c2: p(c2), to: p(a) });
                cur = a;
            }
            PathEl::CurveTo(c1, c2, a) => {
                push_seg(&mut out, cur, vx::Segment::CubicTo { c1: p(c1), c2: p(c2), to: p(a) });
                cur = a;
            }
            PathEl::ClosePath => {
                if let Some(last) = out.last_mut() {
                    last.closed = true;
                }
            }
        }
    }
    for g in &mut out {
        g.no_fill = !g.closed;
    }
    out
}

fn push_seg(out: &mut Vec<vx::GeomPath>, cur: Point, seg: vx::Segment) {
    if out.is_empty() {
        out.push(vx::GeomPath { segments: vec![vx::Segment::MoveTo((cur.x, cur.y))], ..Default::default() });
    }
    if let Some(last) = out.last_mut() {
        last.segments.push(seg);
    }
}

/// Points along `path` about evenly spaced, at most `n` per subpath (for matching outlines).
fn samples(path: &BezPath, n: usize) -> Vec<Point> {
    let mut runs: Vec<Vec<Point>> = Vec::new();
    let tol = path.bounding_box().size().max_side() * 0.002 + 1e-6;
    figcraft_geom::kurbo::flatten(path.elements().iter().copied(), tol, |el| match el {
        PathEl::MoveTo(a) => runs.push(vec![a]),
        PathEl::LineTo(a) => {
            if let Some(r) = runs.last_mut() {
                r.push(a);
            }
        }
        PathEl::ClosePath => {
            if let Some(r) = runs.last_mut()
                && let Some(f) = r.first().copied()
            {
                r.push(f);
            }
        }
        _ => {}
    });
    let mut out = Vec::new();
    for run in runs {
        let len: f64 = run.windows(2).map(|w| (w[1] - w[0]).hypot()).sum();
        if len <= 0.0 {
            out.extend(run.first().copied());
            continue;
        }
        let step = len / n as f64;
        let mut next = 0.0;
        let mut walked = 0.0;
        for w in run.windows(2) {
            let d = (w[1] - w[0]).hypot();
            while next <= walked + d && d > 0.0 {
                out.push(w[0].lerp(w[1], (next - walked) / d));
                next += step;
            }
            walked += d;
        }
    }
    out
}

/// The farthest any sample of either set is from the other set.
fn distance(a: &[Point], b: &[Point]) -> f64 {
    let one_way = |a: &[Point], b: &[Point]| a.iter().map(|p| b.iter().map(|q| (*p - *q).hypot()).fold(f64::INFINITY, f64::min)).fold(0.0, f64::max);
    one_way(a, b).max(one_way(b, a))
}

/// The board outline drawing geometry `paths` in a `w` × `h` box draws, if one does, and whether
/// it draws it exactly (else only nearly: details such as rounded corners are lost).
fn matching_shape(paths: &[vx::GeomPath], w: f64, h: f64) -> Option<(BoardShape, bool)> {
    let segments: usize = paths.iter().map(|p| p.segments.len()).sum();
    if segments == 0 || segments > MAX_MATCH_SEGMENTS {
        return None;
    }
    let bez = geom_to_bez(paths)?;
    let mine = samples(&bez, 48);
    let long = w.max(h);
    BoardShape::ALL
        .into_iter()
        .map(|s| (s, distance(&mine, &samples(&s.path(w, h), 48))))
        .filter(|(_, d)| *d <= NEAR * long)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, d)| (s, d <= SAME * long))
}

// ---------------------------------------------------------------------------------------------
// Colours.

fn rgba(c: Color) -> vx::Rgba {
    let [r, g, b, a] = c.to_rgba8();
    vx::Rgba { r, g, b, a }
}

fn colour(c: vx::Rgba) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn solid(paints: &[Paint]) -> Option<Color> {
    paints.iter().rev().find(|p| p.visible).and_then(Paint::solid_color)
}

// ---------------------------------------------------------------------------------------------
// Export.

/// What was exported: the drawing, its counts and the warnings.
pub struct Exported {
    pub drawing: vx::Drawing,
    pub shapes: usize,
    pub connectors: usize,
    pub warnings: Vec<String>,
}

/// Board page `page` of `doc` as a one-page drawing.
pub fn drawing(doc: &Document, page: NodeId) -> Result<Exported> {
    let mut ex =
        Export { doc, shapes: Vec::new(), ids: BTreeMap::new(), next: 1, left_out: BTreeMap::new(), partial: BTreeMap::new(), notes: Vec::new() };
    let mut connectors = Vec::new();
    ex.walk(page, &mut connectors, 0);
    let mut lines = Vec::new();
    for id in connectors {
        if let Some(c) = ex.connector(id) {
            lines.push(c);
        }
    }
    let mut warnings = ex.notes;
    for (what, n) in &ex.left_out {
        warnings.push(format!("{n} {what} left out: a drawing has no place for them"));
    }
    for (what, n) in &ex.partial {
        warnings.push(format!("{n} {what}"));
    }
    let mut shapes = ex.shapes;
    let (x0, y0, x1, y1) = extent(&shapes, &lines).unwrap_or((0.0, 0.0, 0.0, 0.0));
    let (dx, dy) = (MARGIN - x0, MARGIN - y0);
    for s in &mut shapes {
        s.x += dx;
        s.y += dy;
    }
    for c in &mut lines {
        for p in &mut c.points {
            *p = (p.0 + dx, p.1 + dy);
        }
    }
    let name = doc.node(page).map_or_else(|| "Page 1".to_string(), |n| n.name.clone());
    let (shapes_n, lines_n) = (shapes.len(), lines.len());
    let page = vx::Page { name, width: x1 - x0 + 2.0 * MARGIN, height: y1 - y0 + 2.0 * MARGIN, shapes, connectors: lines };
    Ok(Exported { drawing: vx::Drawing { pages: vec![page] }, shapes: shapes_n, connectors: lines_n, warnings })
}

/// The box around the shapes and the connectors' points (`None`: nothing).
fn extent(shapes: &[vx::Shape], lines: &[vx::Connector]) -> Option<(f64, f64, f64, f64)> {
    let boxes = shapes.iter().map(|s| (s.x, s.y, s.x + s.width, s.y + s.height));
    let points = lines.iter().flat_map(|c| c.points.iter().map(|p| (p.0, p.1, p.0, p.1)));
    boxes.chain(points).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// A layer's box in the drawing.
struct Placed {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    angle: f64,
    /// Takes the layer's own outline into the box.
    fit: Affine,
}

/// Whether the top visible paint is something other than a colour.
fn not_a_colour(paints: &[Paint]) -> bool {
    paints.iter().rev().find(|p| p.visible).is_some_and(|p| p.solid_color().is_none())
}

struct Export<'a> {
    doc: &'a Document,
    shapes: Vec<vx::Shape>,
    /// Each exported layer's shape (index into `shapes`).
    ids: BTreeMap<NodeId, usize>,
    next: u32,
    /// What was left out, counted by kind.
    left_out: BTreeMap<&'static str, usize>,
    /// What was kept in part, counted by kind (the whole message after the count).
    partial: BTreeMap<&'static str, usize>,
    notes: Vec<String>,
}

impl Export<'_> {
    fn id(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    fn skip(&mut self, what: &'static str) {
        *self.left_out.entry(what).or_default() += 1;
    }

    /// Counts one thing carried across only in part: `what` follows the count in the warning.
    fn part(&mut self, what: &'static str) {
        *self.partial.entry(what).or_default() += 1;
    }

    /// Notes what a drawing can't hold of layer `id`: opacity, effects, and fills that aren't
    /// a colour.
    fn lossy(&mut self, id: NodeId) {
        let Some(n) = self.doc.node(id) else { return };
        if n.opacity < 1.0 {
            self.part("layers have an opacity below 100%, which the drawing doesn't keep");
        }
        if !n.effects.is_empty() {
            self.part("layers have effects (shadows, blurs), which the drawing doesn't keep");
        }
        if not_a_colour(&n.fills) || not_a_colour(&n.strokes) {
            self.part("layers have a gradient or image fill or line, drawn with no fill or line");
        }
    }

    /// The layers in `parent`, back to front: sections before what they hold.
    fn walk(&mut self, parent: NodeId, connectors: &mut Vec<NodeId>, depth: usize) {
        let Some(n) = self.doc.node(parent) else { return };
        for c in n.children.clone() {
            let Some(k) = self.doc.node(c) else { continue };
            if !k.visible {
                continue;
            }
            match &k.board {
                Some(BoardItem::Shape { .. }) => self.shape(c),
                Some(BoardItem::Sticky { .. }) => self.filled_box(c, STICKY_NAME, None),
                Some(BoardItem::Connector(_)) => connectors.push(c),
                Some(BoardItem::Label { .. }) => {}
                Some(BoardItem::MindNode) => {
                    self.filled_box(c, "Mind map node", None);
                    self.note("mind map nodes are drawn as rectangles with their text");
                }
                Some(BoardItem::CodeBlock { .. }) => {
                    self.filled_box(c, "Code block", None);
                    self.note("code blocks are drawn as rectangles with their text, without highlighting");
                }
                Some(BoardItem::Stamp { .. }) => self.skip("stamps"),
                Some(BoardItem::Ink) => self.skip("ink strokes"),
                Some(BoardItem::Table) => self.skip("tables"),
                None if matches!(k.kind, NodeKind::Section { .. }) => {
                    self.filled_box(c, SECTION_NAME, Some(k.name.clone()));
                    if depth < MAX_DEPTH {
                        self.walk(c, connectors, depth + 1);
                    } else if !k.children.is_empty() {
                        self.skip("layers inside sections nested more than 32 deep");
                    }
                }
                None if has_image(k) => self.skip("images"),
                None => self.skip("other layers (text, drawings, frames)"),
            }
        }
    }

    fn note(&mut self, msg: &str) {
        if !self.notes.iter().any(|n| n == msg) {
            self.notes.push(msg.to_string());
        }
    }

    /// Layer `id`'s box in the drawing and how its own outline lies in it. Scale is applied to
    /// the box and the outline, a flip to the outline; a skew can't be kept and is counted.
    fn place(&mut self, id: NodeId) -> Option<Placed> {
        let n = self.doc.node(id)?;
        let t = self.doc.world_transform(id);
        let [a, b, c, d, ..] = t.as_coeffs();
        let (sx, sy) = (a.hypot(b), c.hypot(d));
        if !(sx.is_finite() && sy.is_finite() && sx > 1e-9 && sy > 1e-9) {
            self.skip("layers with no size on the page");
            return None;
        }
        if (a * c + b * d).abs() > 1e-6 * sx * sy {
            self.part("layers are skewed, which the drawing can't hold: they are drawn unskewed");
        }
        let (w, h) = (n.width * sx, n.height * sy);
        let centre = t * Point::new(n.width / 2.0, n.height / 2.0);
        let angle = b.atan2(a).to_degrees();
        let angle = if angle.abs() < 1e-9 { 0.0 } else { angle };
        // A flip runs the outline upside down in its box, which turned by `angle` is the mirror.
        let fit = if a * d - b * c < 0.0 { Affine::new([sx, 0.0, 0.0, -sy, 0.0, h]) } else { Affine::scale_non_uniform(sx, sy) };
        Some(Placed { x: centre.x - w / 2.0, y: centre.y - h / 2.0, w, h, angle, fit })
    }

    fn push(&mut self, id: NodeId, mut s: vx::Shape) {
        s.id = self.id();
        self.lossy(id);
        if !s.text.trim().is_empty() {
            self.note("text is exported without its font, size, weight or alignment");
        }
        // Each side's middle: connectors glued to a side keep it.
        s.connections = vec![(s.width / 2.0, 0.0), (s.width, s.height / 2.0), (s.width / 2.0, s.height), (0.0, s.height / 2.0)];
        self.ids.insert(id, self.shapes.len());
        self.shapes.push(s);
    }

    /// A shape with text: its outline, fill, line and text.
    fn shape(&mut self, id: NodeId) {
        let Some(Placed { x, y, w, h, angle, fit }) = self.place(id) else { return };
        let doc = self.doc;
        let Some(n) = doc.node(id) else { return };
        let outline = n.children.iter().copied().find(|c| doc.node(*c).is_some_and(|k| matches!(k.kind, NodeKind::Vector { .. })));
        let mut s = vx::Shape { name: n.name.clone(), x, y, width: w, height: h, angle, ..Default::default() };
        match outline.and_then(|o| doc.node(o).map(|k| (o, k))) {
            Some((_, o)) => {
                let path = match &o.kind {
                    NodeKind::Vector { network } => fit * o.transform * network.stroke_path(),
                    _ => BezPath::new(),
                };
                s.geometry = bez_to_geom(&path);
                s.fill = solid(&o.fills).map(rgba);
                s.line = solid(&o.strokes).map(rgba);
                s.line_weight = o.stroke.weight;
                s.dashed = !o.stroke.dashes.is_empty();
            }
            None => s.geometry = vec![vx::GeomPath::rect(w, h)],
        }
        if s.geometry.is_empty() {
            s.geometry = vec![vx::GeomPath::rect(w, h)];
        }
        if let Some(BoardItem::Shape { shape: BoardShape::Custom }) = n.board {
            s.master = None;
        }
        (s.text, s.text_color) = text(doc, id);
        self.push(id, s);
    }

    /// A rectangle filled with layer `id`'s fill (a sticky, a section) and its text, or `title`.
    fn filled_box(&mut self, id: NodeId, name: &str, title: Option<String>) {
        let Some(Placed { x, y, w, h, angle, .. }) = self.place(id) else { return };
        let Some(n) = self.doc.node(id) else { return };
        let (body, ink) = text(self.doc, id);
        let s = vx::Shape {
            name: name.to_string(),
            x,
            y,
            width: w,
            height: h,
            angle,
            geometry: vec![vx::GeomPath::rect(w, h)],
            text: title.unwrap_or(body),
            fill: solid(&n.fills).map(rgba),
            line: solid(&n.strokes).map(rgba),
            line_weight: if n.strokes.is_empty() { 0.0 } else { n.stroke.weight },
            text_color: ink,
            ..Default::default()
        };
        self.push(id, s);
    }

    /// Connector `id`: its route, the shapes its ends are glued to, arrowheads, line and label.
    fn connector(&mut self, id: NodeId) -> Option<vx::Connector> {
        let doc = self.doc;
        let n = doc.node(id)?;
        let Some(BoardItem::Connector(c)) = &n.board else { return None };
        let NodeKind::Vector { network } = &n.kind else { return None };
        let path = doc.world_transform(id) * network.stroke_path();
        let mut points: Vec<vx::Point> = Vec::new();
        figcraft_geom::kurbo::flatten(path.elements().iter().copied(), 1.0, |el| {
            if let PathEl::MoveTo(p) | PathEl::LineTo(p) = el
                && points.last() != Some(&(p.x, p.y))
            {
                points.push((p.x, p.y));
            }
        });
        if points.len() < 2 {
            self.skip("connectors with no length");
            return None;
        }
        self.lossy(id);
        let from = self.glue(c.from);
        let to = self.glue(c.to);
        let label = label_of(doc, id).map(|l| text(doc, l).0).unwrap_or_default();
        let caps = [n.stroke.cap_start, n.stroke.cap_end];
        if caps.iter().any(|c| !matches!(c, StrokeCap::None | StrokeCap::TriangleArrow | StrokeCap::LineArrow)) {
            self.note("connector ends other than arrows are drawn as arrows");
        }
        let arrow = |c: StrokeCap| if c == StrokeCap::None { 0 } else { vx::ARROW };
        Some(vx::Connector {
            id: self.id(),
            from,
            to,
            points,
            begin_arrow: arrow(n.stroke.cap_start),
            end_arrow: arrow(n.stroke.cap_end),
            line: solid(&n.strokes).map(rgba),
            line_weight: n.stroke.weight,
            dashed: !n.stroke.dashes.is_empty(),
            label,
        })
    }

    /// Where connector end `e` is glued: an exported shape, at a side's middle, a placed point or
    /// anywhere on it.
    fn glue(&mut self, e: End) -> Option<(u32, vx::ConnectionPoint)> {
        let End::Node { node, side, at } = e else { return None };
        let Some(i) = self.ids.get(&node).copied() else {
            self.part("connector ends are glued to layers that weren't exported, so they are free ends");
            return None;
        };
        let s = self.shapes.get_mut(i)?;
        let cp = match (at, side) {
            (Some([fx, fy]), _) => {
                s.connections.push((fx * s.width, fy * s.height));
                vx::ConnectionPoint::Point(u32::try_from(s.connections.len()).ok()?)
            }
            (None, Side::Top) => vx::ConnectionPoint::Point(1),
            (None, Side::Right) => vx::ConnectionPoint::Point(2),
            (None, Side::Bottom) => vx::ConnectionPoint::Point(3),
            (None, Side::Left) => vx::ConnectionPoint::Point(4),
            (None, Side::Auto) => vx::ConnectionPoint::Shape,
        };
        Some((s.id, cp))
    }
}

fn has_image(n: &figcraft_doc::Node) -> bool {
    n.fills.iter().any(|p| matches!(p.kind, figcraft_color::PaintKind::Image(_)))
}

/// Board object `id`'s text and its colour.
fn text(doc: &Document, id: NodeId) -> (String, Option<vx::Rgba>) {
    let Some(t) = text_of(doc, id).and_then(|t| doc.node(t)) else { return (String::new(), None) };
    match &t.kind {
        NodeKind::Text(td) => (td.characters.clone(), solid(&t.fills).map(rgba)),
        _ => (String::new(), None),
    }
}

/// The current board page as VSDX bytes, with the export's counts and warnings.
pub fn vsdx_bytes(doc: &Document, page: NodeId) -> Result<(Vec<u8>, Exported)> {
    let mut ex = drawing(doc, page)?;
    let (bytes, written) = vx::write(&ex.drawing).map_err(|e| EngineError::Other(format!("the drawing can't be written: {e}")))?;
    ex.warnings.extend(written.iter().map(ToString::to_string));
    Ok((bytes, ex))
}

pub(crate) fn export_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.exportVsdx";
    require(s, cmd)?;
    let page = s.page()?;
    let (bytes, ex) = vsdx_bytes(s.doc()?, page)?;
    let mut out = json!({"format": "vsdx", "shapes": ex.shapes, "connectors": ex.connectors, "warnings": ex.warnings});
    match string(p, "path") {
        Some(path) => {
            crate::cmd::file::write(&path, &bytes)?;
            out["path"] = json!(path);
            out["bytes"] = json!(bytes.len());
        }
        None => out["data"] = json!(base64::engine::general_purpose::STANDARD.encode(&bytes)),
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Import.

/// The bytes and file name `cmd` was given: a `path`, or base64 `data` with a `name`.
pub(super) fn file_param(cmd: &str, p: &Value, default_name: &str) -> Result<(Vec<u8>, String)> {
    if let Some(path) = string(p, "path") {
        let bytes = crate::file_access::read_capped(&path, MAX_FILE)?;
        return Ok((bytes, path));
    }
    let Some(d) = string(p, "data") else { return Err(bad(cmd, "pass `path`, or `data` (base64) and `name`")) };
    let d = d.split_once(',').map_or(d.as_str(), |(_, b)| b).trim().to_string();
    if d.len() as u64 > MAX_FILE / 3 * 4 + 4 {
        return Err(bad(cmd, "the file is too large"));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(d).map_err(|e| bad(cmd, format!("`data` isn't base64: {e}")))?;
    Ok((bytes, string(p, "name").unwrap_or_else(|| default_name.to_string())))
}

pub(super) fn import_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.importVsdx";
    require(s, cmd)?;
    let (bytes, _) = file_param(cmd, p, "drawing.vsdx")?;
    let at = match (num(p, "x"), num(p, "y")) {
        (None, None) => None,
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => return Err(bad(cmd, "`x` and `y` are finite numbers, given together")),
    };
    let index = match get_index(p) {
        Ok(i) => i,
        Err(msg) => return Err(bad(cmd, msg)),
    };
    import_bytes(s, &bytes, index, at)
}

fn get_index(p: &Value) -> std::result::Result<usize, &'static str> {
    match p.get("page") {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or("`page` is a page's index, from 0"),
    }
}

/// Drawing `bytes`' page `index` added to the current board page, its top-left at `at` (`None`:
/// centred in the view): one undo step when run as a command.
pub(crate) fn import_bytes(s: &mut Session, bytes: &[u8], index: usize, at: Option<(f64, f64)>) -> Result<Value> {
    let cmd = "board.importVsdx";
    let (drawing, read_warnings) = vx::read_with(bytes, &vx::Limits::DEFAULT).map_err(|e| {
        if e.is_limit() {
            bad(cmd, format!("the drawing is larger than can be read ({e}): nothing was added"))
        } else {
            bad(cmd, format!("this isn't a VSDX drawing that can be read ({e}): nothing was added"))
        }
    })?;
    let mut warnings: Vec<String> = read_warnings.iter().map(ToString::to_string).collect();
    let pages = drawing.pages.len();
    let Some(page) = drawing.pages.into_iter().nth(index) else {
        return Err(bad(cmd, format!("the drawing has {pages} page(s), so there's no page {index}: nothing was added")));
    };
    if pages > 1 {
        warnings.push(format!("the drawing has {pages} pages: only page {} (`{}`) was added", index + 1, short(&page.name)));
    }
    if page.shapes.is_empty() && page.connectors.is_empty() {
        return Err(bad(cmd, "the drawing's page is empty: nothing was added"));
    }
    let (x0, y0, x1, y1) = extent(&page.shapes, &page.connectors).unwrap_or((0.0, 0.0, 0.0, 0.0));
    let origin = match at {
        Some(p) => p,
        None => {
            let v = &s.ds()?.view;
            (v.cx - (x1 - x0) / 2.0, v.cy - (y1 - y0) / 2.0)
        }
    };
    let offset = Vec2::new(origin.0 - x0, origin.1 - y0);
    let board_page = s.page()?;
    let by_id: HashMap<u32, &vx::Shape> = page.shapes.iter().map(|sh| (sh.id, sh)).collect();
    let mut im = Import {
        s,
        page: board_page,
        offset,
        made: Vec::new(),
        ids: BTreeMap::new(),
        sections: Vec::new(),
        warnings,
        counts: BTreeMap::new(),
        by_id,
    };
    let mut counts = (0, 0, 0, 0);
    let mut room = MAX_LAYERS;
    for shape in &page.shapes {
        if room == 0 {
            im.count("shapes were left out: a page imports at most 5000 shapes and connectors");
            continue;
        }
        room -= 1;
        match im.shape(shape) {
            Ok(Kind::Shape) => counts.0 += 1,
            Ok(Kind::Section) => counts.2 += 1,
            Ok(Kind::Sticky) => counts.3 += 1,
            Err(e) => im.warnings.push(format!("shape {} (`{}`): not added ({e})", shape.id, short(&shape.name))),
        }
    }
    for c in &page.connectors {
        if room == 0 {
            im.count("connectors were left out: a page imports at most 5000 shapes and connectors");
            continue;
        }
        room -= 1;
        match im.connector(c) {
            Ok(()) => counts.1 += 1,
            Err(e) => im.warnings.push(format!("connector {}: not added ({e})", c.id)),
        }
    }
    im.finish();
    let Import { made, warnings, .. } = im;
    if made.is_empty() {
        return Err(bad(cmd, format!("nothing on the drawing's page could be added ({})", warnings.join("; "))));
    }
    s.set_selection(made.clone());
    // The app shows this; the result lists them all.
    s.message = match warnings.as_slice() {
        [] => None,
        [one] => Some(format!("VSDX drawing: {one}")),
        [first, ..] => Some(format!("VSDX drawing: {} things couldn't be carried across exactly, e.g. {first}", warnings.len())),
    };
    Ok(json!({
        "ids": made,
        "shapes": counts.0,
        "connectors": counts.1,
        "sections": counts.2,
        "stickies": counts.3,
        "warnings": warnings,
    }))
}

enum Kind {
    Shape,
    Section,
    Sticky,
}

struct Import<'a, 'p> {
    s: &'a mut Session,
    /// The page's shapes by drawing id.
    by_id: HashMap<u32, &'p vx::Shape>,
    /// What was carried across in part, counted by kind (the words after the count).
    counts: BTreeMap<&'static str, usize>,
    page: NodeId,
    offset: Vec2,
    made: Vec<NodeId>,
    /// Each drawing shape's layer.
    ids: BTreeMap<u32, NodeId>,
    /// The sections made, with their page boxes (newest last).
    sections: Vec<(NodeId, Rect)>,
    warnings: Vec<String>,
}

/// Runs board command `cmd` inside this command's undo step: the layer it made.
pub(super) fn run(s: &mut Session, cmd: &str, p: &Value) -> Result<NodeId> {
    let spec = crate::cmd::find_command(cmd).ok_or_else(|| EngineError::UnknownCommand(cmd.into()))?;
    let v = (spec.run)(s, p)?;
    v.get("id").and_then(id_of).ok_or_else(|| EngineError::Other(format!("`{cmd}` made no layer")))
}

/// `text` cut to a length a warning can hold.
fn short(text: &str) -> String {
    const MAX: usize = 48;
    if text.chars().nth(MAX).is_some() { format!("{}...", text.chars().take(MAX).collect::<String>()) } else { text.to_string() }
}

/// A size a board object can have.
fn size_ok(v: f64) -> bool {
    v.is_finite() && (1.0..=100_000.0).contains(&v)
}

impl Import<'_, '_> {
    /// Counts one thing that didn't come across exactly: `what` follows the count in its warning.
    fn count(&mut self, what: &'static str) {
        *self.counts.entry(what).or_default() += 1;
    }

    /// Turns the counts into one warning per kind.
    fn finish(&mut self) {
        for (what, n) in std::mem::take(&mut self.counts) {
            self.warnings.push(format!("{n} {what}"));
        }
    }

    /// Where a box at drawing point `(x, y)` goes: the innermost section holding all of `r`, and
    /// the place in it (or on the page).
    fn parent_for(&self, r: Rect) -> (NodeId, Point) {
        for (id, b) in self.sections.iter().rev() {
            if b.contains_rect(r) {
                return (*id, Point::new(r.x0 - b.x0, r.y0 - b.y0));
            }
        }
        (self.page, Point::new(r.x0, r.y0))
    }

    fn shape(&mut self, sh: &vx::Shape) -> Result<Kind> {
        if !(size_ok(sh.width.max(1.0)) && size_ok(sh.height.max(1.0))) {
            return Err(EngineError::Other("its size is out of range".into()));
        }
        if sh.width < 1.0 || sh.height < 1.0 {
            self.count("shapes were under 1 point across and were enlarged to 1");
        }
        let (w, h) = (sh.width.max(1.0), sh.height.max(1.0));
        let at = Point::new(sh.x, sh.y) + self.offset;
        if !(at.x.is_finite() && at.y.is_finite()) {
            return Err(EngineError::Other("its place isn't a number".into()));
        }
        let r = Rect::new(at.x, at.y, at.x + w, at.y + h);
        let (parent, local) = self.parent_for(r);
        let fill = sh.fill.map(colour);
        let base = sh.name.split('.').next().unwrap_or("");
        let plain_box = sh.angle == 0.0 && matches!(matching_shape(&sh.geometry, w, h), Some((BoardShape::Rectangle, true)));
        let (id, kind) = if base == SECTION_NAME && plain_box {
            let title = if sh.text.trim().is_empty() { SECTION_TITLE.to_string() } else { sh.text.trim().to_string() };
            let mut p = json!({"type": "section", "name": title, "width": w, "height": h, "parent": parent, "x": local.x, "y": local.y});
            if let Some(c) = fill {
                p["props"] = json!({"fills": [c.to_hex()]});
            }
            let id = run(self.s, "node.create", &p)?;
            self.sections.push((id, r));
            (id, Kind::Section)
        } else if base == STICKY_NAME && plain_box {
            let mut p = json!({"text": sh.text, "parent": parent, "x": local.x, "y": local.y, "author": false});
            if let Some(c) = fill {
                p["color"] = json!(c.to_hex());
            }
            (run(self.s, "board.sticky", &p)?, Kind::Sticky)
        } else {
            (self.outline_shape(sh, parent, local, (w, h))?, Kind::Shape)
        };
        if parent == self.page {
            self.made.push(id);
        }
        self.ids.insert(sh.id, id);
        Ok(kind)
    }

    /// A shape with text for drawing shape `sh`: a board outline when its geometry draws one, else
    /// its own outline.
    fn outline_shape(&mut self, sh: &vx::Shape, parent: NodeId, at: Point, (w, h): (f64, f64)) -> Result<NodeId> {
        let found = matching_shape(&sh.geometry, w, h);
        let matched = found.map(|(b, _)| b);
        if matches!(found, Some((_, false))) {
            self.count("shapes only resemble a board shape and are drawn as it, so details such as rounded corners are lost");
        }
        if sh.geometry.iter().any(|g| g.no_line || (g.no_fill && g.closed)) {
            self.count("shapes have parts with no line or no fill of their own, which are drawn with the shape's line and fill");
        }
        let fill = sh.fill.map(colour);
        let p = json!({
            "shape": matched.unwrap_or(BoardShape::Rectangle).id(),
            "width": w, "height": h, "text": sh.text,
            "color": fill.unwrap_or(Color::WHITE).to_hex(),
            "parent": parent, "x": at.x, "y": at.y,
        });
        let id = run(self.s, "board.shape", &p)?;
        match self.finish_shape(id, sh, matched, at, (w, h)) {
            Ok(()) => Ok(id),
            Err(e) => {
                // Not added means not left behind.
                if !self.s.doc_mut().is_ok_and(|doc| doc.remove(id).is_ok()) {
                    self.count("shapes failed part-way and could not be taken off the board again");
                }
                Err(e)
            }
        }
    }

    /// Gives the new shape layer `id` its outline, style and turn.
    fn finish_shape(&mut self, id: NodeId, sh: &vx::Shape, matched: Option<BoardShape>, at: Point, (w, h): (f64, f64)) -> Result<()> {
        let mut outline = None;
        if matched.is_none() {
            let segments: usize = sh.geometry.iter().map(|g| g.segments.len()).sum();
            let path = if sh.geometry.is_empty() {
                Some(Rect::new(0.0, 0.0, w, h).to_path(0.1))
            } else if segments > MAX_OUTLINE_SEGMENTS {
                self.warnings.push(format!("shape {}: its outline has {segments} segments and is drawn as its box", sh.id));
                Some(Rect::new(0.0, 0.0, w, h).to_path(0.1))
            } else {
                let bez = geom_to_bez(&sh.geometry);
                if bez.is_none() {
                    self.count("shapes have an outline with a value that isn't a number and are drawn as their box");
                }
                bez
            };
            outline = Some(path.unwrap_or_else(|| Rect::new(0.0, 0.0, w, h).to_path(0.1)));
        }
        let doc = self.s.doc_mut()?;
        if let Some(path) = outline {
            let name = sh.master.clone().filter(|m| !m.trim().is_empty());
            custom_outline(doc, id, &path, name.as_deref())?;
        }
        style_shape(doc, id, sh);
        if sh.angle != 0.0
            && sh.angle.is_finite()
            && let Some(n) = doc.node_mut(id)
        {
            let centre = Vec2::new(at.x + w / 2.0, at.y + h / 2.0);
            n.transform = Affine::translate(centre) * Affine::rotate(sh.angle.to_radians()) * Affine::translate((-w / 2.0, -h / 2.0));
        }
        Ok(())
    }

    /// Connector `c`: glued ends attached to their shapes, free ones at their points.
    fn connector(&mut self, c: &vx::Connector) -> Result<()> {
        let (Some(first), Some(last)) = (c.points.first(), c.points.last()) else {
            return Err(EngineError::Other("it has no route".into()));
        };
        let from = self.end(c.from, *first, c.id);
        let to = self.end(c.to, *last, c.id);
        let points: Vec<Point> = c.points.iter().map(|p| pt(*p) + self.offset).collect();
        let orthogonal = points.len() >= 3 && points.windows(2).all(|w| (w[0].x - w[1].x).abs() < 0.01 || (w[0].y - w[1].y).abs() < 0.01);
        let glued = matches!(from, End::Node { .. }) || matches!(to, End::Node { .. });
        if points.len() > 2 && !orthogonal {
            self.warnings.push(format!("connector {}: its bends aren't at right angles, so it is drawn straight", c.id));
        }
        let style = if orthogonal { "elbow" } else { "straight" };
        if [c.begin_arrow, c.end_arrow].iter().any(|a| !matches!(*a, 0 | vx::ARROW)) {
            self.count("connectors have arrowheads of other kinds, drawn as triangle arrowheads");
        }
        let cap = |a: u8| if a == 0 { "none" } else { "triangleArrow" };
        let mut p = json!({
            "from": end_json(from), "to": end_json(to), "style": style,
            "startCap": cap(c.begin_arrow), "endCap": cap(c.end_arrow),
        });
        for (key, e) in [("fromSide", from), ("toSide", to)] {
            if let End::Node { side, .. } = e {
                p[key] = json!(side);
            }
        }
        for (key, e) in [("fromAt", from), ("toAt", to)] {
            if let End::Node { at: Some(at), .. } = e {
                p[key] = json!(at);
            }
        }
        if !c.label.trim().is_empty() {
            p["label"] = json!(c.label);
        }
        let id = run(self.s, "board.connector", &p)?;
        self.made.push(id);
        let doc = self.s.doc_mut()?;
        if orthogonal && !glued {
            keep_bends(doc, id, &points);
        }
        if let Some(n) = doc.node_mut(id) {
            match c.line {
                Some(l) => n.strokes = vec![Paint::solid(colour(l))],
                None => n.strokes.clear(),
            }
            if c.line_weight.is_finite() && c.line_weight > 0.0 {
                n.stroke.weight = c.line_weight.min(100.0);
            }
            if c.dashed {
                n.stroke.dashes = DASHES.to_vec();
            }
        }
        Ok(())
    }

    /// A connector end: glued to drawing shape `glue` (its side or connection point), or free at
    /// `point`.
    fn end(&mut self, glue: Option<(u32, vx::ConnectionPoint)>, point: vx::Point, who: u32) -> End {
        let free = || {
            let p = pt(point) + self.offset;
            End::Point { x: p.x, y: p.y }
        };
        let Some((sid, cp)) = glue else { return free() };
        let Some(node) = self.ids.get(&sid).copied() else {
            self.warnings.push(format!("connector {who}: the shape {sid} it's glued to wasn't added, so that end is free"));
            return free();
        };
        let vx::ConnectionPoint::Point(n) = cp else { return End::Node { node, side: Side::Auto, at: None } };
        let Some(sh) = self.by_id.get(&sid).copied() else { return End::Node { node, side: Side::Auto, at: None } };
        let Some(c) = usize::try_from(n).ok().and_then(|n| n.checked_sub(1)).and_then(|i| sh.connections.get(i)) else {
            return End::Node { node, side: Side::Auto, at: None };
        };
        let (fx, fy) = ((c.0 / sh.width.max(1e-9)).clamp(0.0, 1.0), (c.1 / sh.height.max(1e-9)).clamp(0.0, 1.0));
        let near = |a: f64, b: f64| (a - b).abs() < 0.01;
        let side = [(fy, Side::Top), (1.0 - fx, Side::Right), (1.0 - fy, Side::Bottom), (fx, Side::Left)]
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Side::Auto, |(_, s)| s);
        let middle = match side {
            Side::Top | Side::Bottom => near(fx, 0.5),
            Side::Left | Side::Right => near(fy, 0.5),
            Side::Auto => false,
        };
        End::Node { node, side, at: (!middle).then_some([fx, fy]) }
    }
}

fn end_json(e: End) -> Value {
    match e {
        End::Node { node, .. } => json!(node),
        End::Point { x, y } => json!({"x": x, "y": y}),
    }
}

/// A free connector keeps the right-angled route `pts` it came with.
fn keep_bends(doc: &mut Document, id: NodeId, pts: &[Point]) {
    let (Some(first), Some(end)) = (pts.first(), pts.last()) else { return };
    if pts.len() < 3 || pts.len() > figcraft_doc::MAX_RUNS {
        return;
    }
    let mid = first.midpoint(*end);
    let across = |i: usize| pts.get(i).zip(pts.get(i + 1)).is_some_and(|(a, b)| (a.y - b.y).abs() < 0.01);
    let at: Vec<f64> = (1..pts.len() - 2).filter_map(|i| pts.get(i).map(|q| if across(i) { q.y - mid.y } else { q.x - mid.x })).collect();
    let Some(BoardItem::Connector(mut c)) = doc.node(id).and_then(|n| n.board.clone()) else { return };
    c.path = Some(ConnectorPath::Runs { across: across(0), at });
    let routing = c.routing;
    let route_pts = route(doc, &c);
    if let Some(n) = doc.node_mut(id) {
        n.board = Some(BoardItem::Connector(c));
    }
    if let Some(r) = route_pts {
        place_route(doc, id, &r, routing);
    }
}

/// Drawing shape `sh`'s line and text colour on shape `id`; no fill or line where it has none.
fn style_shape(doc: &mut Document, id: NodeId, sh: &vx::Shape) {
    let children = doc.node(id).map(|n| n.children.clone()).unwrap_or_default();
    for c in children {
        let Some(n) = doc.node_mut(c) else { continue };
        match n.kind {
            NodeKind::Vector { .. } => {
                if sh.fill.is_none() {
                    n.fills.clear();
                }
                match sh.line {
                    Some(l) => n.strokes = vec![Paint::solid(colour(l))],
                    None => n.strokes.clear(),
                }
                if sh.line_weight.is_finite() && sh.line_weight > 0.0 {
                    n.stroke.weight = sh.line_weight.min(100.0);
                }
                if sh.dashed {
                    n.stroke.dashes = DASHES.to_vec();
                }
            }
            NodeKind::Text(_) => {
                if let Some(c) = sh.text_color {
                    n.fills = vec![Paint::solid(colour(c))];
                }
            }
            _ => {}
        }
    }
}

/// Shape with text `id` takes outline `path` (in its box) of its own: a custom shape, named
/// `name` when it has one.
pub(super) fn custom_outline(doc: &mut Document, id: NodeId, path: &BezPath, name: Option<&str>) -> Result<()> {
    let n = doc.get_mut(id)?;
    n.board = Some(BoardItem::Shape { shape: BoardShape::Custom });
    n.name = name.unwrap_or(BoardShape::Custom.label()).to_string();
    let (w, h) = (n.width, n.height);
    if let Some(l) = n.layout.as_mut() {
        l.padding = shape_padding(BoardShape::Custom, w, h);
    }
    let outline = doc.get(id)?.children.iter().copied().find(|c| doc.node(*c).is_some_and(|k| matches!(k.kind, NodeKind::Vector { .. })));
    if let Some(o) = outline
        && let Some(on) = doc.node_mut(o)
    {
        on.kind = NodeKind::Vector { network: VectorNetwork::from_path(path) };
        (on.width, on.height, on.transform) = (w, h, Affine::IDENTITY);
    }
    Ok(())
}
