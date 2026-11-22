//! Diagrams from text and tidy layouts: `board.importMermaid` turns a Mermaid flowchart into
//! shapes, sections and elbow connectors laid out in ranks, and `board.tidyDiagram` lays out shapes
//! already on the board along the connectors between them. Both are one undo step.

use std::collections::BTreeMap;

use craft_graph_layout as gl;
use craft_mermaid as mm;
use figcraft_color::{Color, Paint};
use figcraft_doc::{BoardItem, BoardShape, Document, End, NodeId, NodeKind, Side, TextData};
use figcraft_geom::{Affine, Vec2};
use serde_json::{Value, json};

use super::connectors;
use super::derived::{place_route, route};
use super::{Result, Session, bad, id_of, num, require, string};
use crate::EngineError;
use crate::params::targets;

/// Text size of a shape's label (what `board.shape` uses) and of a connector's label.
const TEXT_SIZE: f64 = 16.0;
const LABEL_SIZE: f64 = 14.0;
/// Room between a label and its shape's outline.
const PAD: (f64, f64) = (14.0, 10.0);
/// The smallest shape an imported node gets.
const MIN_SIZE: (f64, f64) = (96.0, 48.0);
/// Wider labels wrap.
const MAX_TEXT_WIDTH: f64 = 240.0;
/// Space inside a section around what it holds, and the band its title takes.
const SECTION_PADDING: f64 = 24.0;
const SECTION_TITLE: f64 = 40.0;
/// Room between shapes and between ranks.
const NODE_SEP: f64 = 48.0;
const RANK_SEP: f64 = 64.0;
/// What an imported shape and section are filled with when the text sets no colour.
const SHAPE_FILL: &str = "#FFFFFF";
const SECTION_FILL: &str = "#F0EFFA";
/// The most nodes and links one import takes: past this, laying out and routing would stall the app.
const MAX_NODES: usize = 500;
const MAX_EDGES: usize = 2_000;
/// A thick (`==>`) line's weight and a dotted (`-.->`) line's dashes.
const THICK: f64 = 4.0;
const DOTTED: [f64; 2] = [4.0, 4.0];

/// Whether `text` reads as a Mermaid flowchart: after any front matter, blank lines and `%%`
/// comments, its first line is `flowchart` or `graph`, optionally with a direction (`TB`, `TD`,
/// `BT`, `LR` or `RL`) and a `;`, and nothing else. What pasting on a board imports.
pub fn looks_like_flowchart(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("%%")).peekable();
    if lines.peek() == Some(&"---") {
        lines.next();
        for l in lines.by_ref() {
            if l == "---" {
                break;
            }
        }
    }
    let Some(first) = lines.next() else { return false };
    let mut words = first.strip_suffix(';').unwrap_or(first).split_whitespace();
    matches!(words.next(), Some("flowchart" | "graph"))
        && words.next().is_none_or(|d| matches!(d, "TB" | "TD" | "BT" | "LR" | "RL"))
        && words.next().is_none()
}

pub(super) fn import_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.importMermaid";
    require(s, cmd)?;
    let text = string(p, "text").unwrap_or_default();
    if text.trim().is_empty() {
        return Err(bad(cmd, "`text` is a Mermaid flowchart (it starts with `flowchart` or `graph`)"));
    }
    let chart = match mm::parse(&text) {
        Ok(mm::Diagram::Flowchart(f)) => f,
        Ok(mm::Diagram::Unsupported { kind }) => {
            return Err(bad(cmd, format!("only flowcharts can be imported, not `{kind}`: nothing was added")));
        }
        Err(e) => return Err(bad(cmd, format!("the text isn't a flowchart that can be read ({e}): nothing was added"))),
    };
    if chart.nodes.is_empty() {
        return Err(bad(cmd, "the flowchart has no nodes: nothing was added"));
    }
    if chart.nodes.len() > MAX_NODES || chart.edges.len() > MAX_EDGES {
        return Err(bad(
            cmd,
            format!(
                "the flowchart has {} nodes and {} links, more than the {MAX_NODES} nodes and {MAX_EDGES} links that can be imported at once: nothing was added",
                chart.nodes.len(),
                chart.edges.len()
            ),
        ));
    }
    let mut warnings: Vec<String> = chart.warnings.iter().map(|w| format!("line {}: {}", w.line, w.message)).collect();
    let plan = plan(&chart, &mut warnings);
    let layout = gl::layered(
        &plan.graph,
        &gl::LayeredOptions { direction: direction(chart.direction), node_sep: NODE_SEP, rank_sep: RANK_SEP, ..Default::default() },
    )
    .map_err(|e| bad(cmd, format!("the flowchart can't be laid out ({e}): nothing was added")))?;
    let view = &s.ds()?.view;
    let at = (num(p, "x").unwrap_or(view.cx - layout.size.width / 2.0), num(p, "y").unwrap_or(view.cy - layout.size.height / 2.0));
    if !(at.0.is_finite() && at.1.is_finite()) {
        return Err(bad(cmd, "`x` and `y` are finite numbers"));
    }
    let page = s.page()?;
    let mut b = Build { s, page, at, made: Vec::new() };

    // Sections, outer ones first, each inside the one around it.
    let mut sections: Vec<Option<NodeId>> = vec![None; chart.subgraphs.len()];
    for i in by_depth(&chart.subgraphs) {
        let (Some(sg), Some(r)) = (chart.subgraphs.get(i), layout.groups.get(i)) else { continue };
        let parent = sg.parent.and_then(|p| sections.get(p).copied().flatten());
        let origin = sg.parent.and_then(|p| layout.groups.get(p)).map_or((0.0, 0.0), |q| (q.x, q.y));
        let fill = sg.style.fill.as_deref().and_then(Color::from_hex_or_name).map_or(SECTION_FILL.to_string(), |c| c.to_hex());
        if sg.title_markdown {
            warnings.push(format!("subgraph `{}`: its title's markdown is shown as plain text", sg.id));
        }
        let title = if sg.title.trim().is_empty() { sg.id.clone() } else { sg.title.clone() };
        let id = b.place(
            "node.create",
            parent,
            (r.x - origin.0, r.y - origin.1),
            json!({"type": "section", "name": title, "width": r.width.max(1.0), "height": r.height.max(1.0), "props": {"fills": [fill]}}),
        )?;
        sections[i] = Some(id);
    }

    // Shapes, in their sections.
    let mut shapes = Vec::with_capacity(chart.nodes.len());
    for (i, node) in chart.nodes.iter().enumerate() {
        let (Some(r), Some(spec)) = (layout.nodes.get(i), plan.shapes.get(i)) else { continue };
        let group = plan.graph.nodes.get(i).and_then(|n| n.group);
        let parent = group.and_then(|g| sections.get(g).copied().flatten());
        let origin = group.and_then(|g| layout.groups.get(g)).map_or((0.0, 0.0), |q| (q.x, q.y));
        let style = chart.node_style(i);
        let fill = colour(&style.fill, "fill", &node.id, &mut warnings).map_or(SHAPE_FILL.to_string(), |c| c.to_hex());
        let id = b.place(
            "board.shape",
            parent,
            (r.x - origin.0, r.y - origin.1),
            json!({"shape": spec.id(), "width": r.width, "height": r.height, "text": node.label, "color": fill}),
        )?;
        style_shape(b.s.doc_mut()?, id, &style, &node.id, &mut warnings);
        shapes.push(id);
    }

    // Connectors, elbows leaving and arriving on the sides the layout chose.
    let mut connectors = 0;
    for (i, edge) in chart.edges.iter().enumerate() {
        let name = || format!("link {} → {}", end_name(&chart, edge.from), end_name(&chart, edge.to));
        if edge.stroke == mm::Stroke::Invisible {
            continue;
        }
        let end = |e: mm::Endpoint| match e {
            mm::Endpoint::Node(n) => shapes.get(n).copied(),
            mm::Endpoint::Subgraph(g) => sections.get(g).copied().flatten(),
        };
        let (Some(from), Some(to)) = (end(edge.from), end(edge.to)) else { continue };
        if from == to {
            warnings.push(format!("{}: a link from a node to itself isn't drawn", name()));
            continue;
        }
        let (from_side, to_side) = plan.edges.get(i).copied().flatten().map_or(("auto", "auto"), |k| sides(&layout, &plan.graph, k));
        let mut cp = json!({
            "from": from, "to": to, "fromSide": from_side, "toSide": to_side, "style": "elbow",
            "startCap": cap(edge.start_head, &name, &mut warnings), "endCap": cap(edge.end_head, &name, &mut warnings),
        });
        if let Some(label) = edge.label.as_deref().filter(|l| !l.trim().is_empty()) {
            cp["label"] = json!(label);
        }
        let id = match connectors::create_unrouted(b.s, &cp) {
            Ok(id) => id,
            Err(e) => {
                warnings.push(format!("{}: not drawn ({e})", name()));
                continue;
            }
        };
        b.made.push(id);
        connectors += 1;
        style_line(b.s.doc_mut()?, id, edge.stroke, &chart.edge_style(i), &name(), &mut warnings);
    }

    let made = b.made;
    s.set_selection(made.clone());
    Ok(json!({
        "ids": made,
        "shapes": shapes.len(),
        "connectors": connectors,
        "sections": sections.iter().flatten().count(),
        "warnings": warnings,
    }))
}

pub(super) fn tidy_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.tidyDiagram";
    require(s, cmd)?;
    let dir = match string(p, "direction") {
        None => gl::Direction::Down,
        Some(v) => direction_of(&v).ok_or_else(|| bad(cmd, format!("`direction` is down, up, right or left, not `{v}`")))?,
    };
    let page = s.page()?;
    let named = p.get("ids").is_some_and(|v| !v.is_null()) || p.get("id").is_some_and(|v| !v.is_null());
    let given = targets(s, p);
    let doc = s.doc()?;
    let mut warnings: Vec<String> = Vec::new();
    let mut items: Vec<NodeId> = given.iter().copied().filter(|c| tidies(doc, *c)).collect();
    let mut holder = page;
    // A selection without two nodes in it (a connector, nothing) tidies the whole page.
    if items.len() < 2 && !named {
        items = doc.node(page).map(|n| n.children.iter().copied().filter(|c| tidies(doc, *c)).collect()).unwrap_or_default();
        let rest = doc.node(page).map_or(0, |n| n.children.iter().filter(|c| !tidies(doc, **c) && !is_line(doc, **c)).count());
        if rest > 0 {
            warnings.push(format!("{rest} layers (ink, tables, images, locked or hidden ones) stay where they are"));
        }
    } else {
        let parents: Vec<Option<NodeId>> = items.iter().map(|i| doc.node(*i).and_then(|n| n.parent)).collect();
        if parents.windows(2).any(|w| w.first() != w.get(1)) {
            return Err(bad(
                cmd,
                "the selected layers are in different places (a section and what is inside it, say): select layers that share one parent",
            ));
        }
        holder = parents.first().copied().flatten().unwrap_or(page);
        let skipped = given.iter().filter(|c| !tidies(doc, **c) && !is_line(doc, **c)).count();
        if skipped > 0 {
            warnings.push(format!("{skipped} selected layers (ink, tables, images, locked or hidden ones) stay where they are"));
        }
    }
    if items.len() < 2 {
        return Err(bad(cmd, "two or more shapes, stickies or sections are needed to tidy"));
    }
    // What moves is laid out on the page and moved in its parent's space: that must be a plain
    // scale and shift.
    let to_page = doc.world_transform(holder);
    let [sx, kx, ky, sy, _, _] = to_page.as_coeffs();
    if kx.abs() > 1e-9 || ky.abs() > 1e-9 || sx <= 1e-9 || sy <= 1e-9 {
        return Err(bad(cmd, "what is inside a rotated or flipped layer can't be tidied: nothing was moved"));
    }
    let index: BTreeMap<NodeId, usize> = items.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let rects: Vec<figcraft_geom::Rect> = items.iter().map(|n| doc.world_bounds(*n).ok_or(EngineError::NoNode(*n))).collect::<Result<_>>()?;
    let mut graph = gl::Graph {
        nodes: rects.iter().map(|r| gl::NodeSpec { size: gl::Size::new(r.width(), r.height()), group: None }).collect(),
        ..Default::default()
    };
    // The item an end belongs to: the layer itself or the section around it.
    let item_of = |n: NodeId| std::iter::once(n).chain(doc.ancestors(n)).find_map(|a| index.get(&a).map(|i| (*i, a == n)));
    // The connectors between them, in the order they are on the page.
    let mut lines: Vec<(NodeId, usize, bool, bool)> = Vec::new();
    for c in doc.subtree(page) {
        let Some(BoardItem::Connector(k)) = doc.node(c).and_then(|n| n.board.as_ref()) else { continue };
        // A mind map's branches follow their own layout.
        if k.branch {
            continue;
        }
        let (Some((f, direct_f)), Some((t, direct_t))) = (k.from.node().and_then(item_of), k.to.node().and_then(item_of)) else { continue };
        if f != t {
            lines.push((c, graph.edges.len(), direct_f, direct_t));
            graph.edges.push(gl::EdgeSpec::new(f, t));
        }
    }
    let layout = gl::layered(&graph, &gl::LayeredOptions { direction: dir, node_sep: NODE_SEP, rank_sep: RANK_SEP, ..Default::default() })
        .map_err(|e| bad(cmd, format!("these can't be laid out ({e})")))?;
    let anchor = rects.iter().fold((f64::INFINITY, f64::INFINITY), |(x, y), r| (x.min(r.x0), y.min(r.y0)));
    let doc = s.doc_mut()?;
    for (i, id) in items.iter().enumerate() {
        let (Some(r), Some(old)) = (layout.nodes.get(i), rects.get(i)) else { continue };
        let delta = Vec2::new((anchor.0 + r.x - old.x0) / sx, (anchor.1 + r.y - old.y0) / sy);
        let n = doc.get_mut(*id)?;
        n.transform = Affine::translate(delta) * n.transform;
    }
    // Each connector leaves and arrives on the sides that suit the new layout (a layer inside a
    // section keeps its side choice automatic: the section is what moved).
    for (id, k, direct_f, direct_t) in &lines {
        let Some(BoardItem::Connector(mut c)) = doc.node(*id).and_then(|n| n.board.clone()) else { continue };
        let (fs, ts) = sides(&layout, &graph, *k);
        set_side(&mut c.from, if *direct_f { fs } else { "auto" });
        set_side(&mut c.to, if *direct_t { ts } else { "auto" });
        c.path = None;
        let routing = c.routing;
        let pts = route(doc, &c);
        doc.get_mut(*id)?.board = Some(BoardItem::Connector(c));
        if let Some(pts) = pts {
            place_route(doc, *id, &pts, routing);
        }
    }
    s.set_selection(items.clone());
    Ok(json!({"ids": items, "connectors": lines.len(), "warnings": warnings}))
}

/// What `board.tidyDiagram` moves: shapes, stickies and sections.
fn tidies(doc: &Document, id: NodeId) -> bool {
    doc.node(id).is_some_and(|n| {
        n.visible
            && !n.locked
            && (matches!(n.board, Some(BoardItem::Shape { .. } | BoardItem::Sticky { .. })) || matches!(n.kind, NodeKind::Section { .. }))
    })
}

/// A connector or its label: not something tidy leaves behind, it follows what it joins.
fn is_line(doc: &Document, id: NodeId) -> bool {
    doc.node(id).is_some_and(|n| matches!(n.board, Some(BoardItem::Connector(_) | BoardItem::Label { .. })))
}

fn set_side(end: &mut End, side: &str) {
    if let End::Node { side: s, at, .. } = end {
        *s = match side {
            "top" => Side::Top,
            "right" => Side::Right,
            "bottom" => Side::Bottom,
            "left" => Side::Left,
            _ => Side::Auto,
        };
        *at = None;
    }
}

/// Runs board commands inside this command's undo step and keeps what it put on the page.
struct Build<'a> {
    s: &'a mut Session,
    page: NodeId,
    at: (f64, f64),
    made: Vec<NodeId>,
}

impl Build<'_> {
    fn run(&mut self, cmd: &str, p: Value) -> Result<NodeId> {
        let spec = crate::cmd::find_command(cmd).ok_or_else(|| EngineError::UnknownCommand(cmd.into()))?;
        let v = (spec.run)(self.s, &p)?;
        v.get("id").and_then(id_of).ok_or_else(|| EngineError::Other(format!("`{cmd}` made no layer")))
    }

    /// A layer at `(x, y)` in `parent`, or from the diagram's corner on the page.
    fn place(&mut self, cmd: &str, parent: Option<NodeId>, (x, y): (f64, f64), mut p: Value) -> Result<NodeId> {
        let (parent, x, y) = match parent {
            Some(sec) => (sec, x, y),
            None => (self.page, self.at.0 + x, self.at.1 + y),
        };
        p["parent"] = json!(parent);
        p["x"] = json!(x);
        p["y"] = json!(y);
        let id = self.run(cmd, p)?;
        if parent == self.page {
            self.made.push(id);
        }
        Ok(id)
    }
}

/// The layout input for a flowchart: a node per Mermaid node, a group per subgraph, an edge per
/// link (a link to a subgraph goes to its first node), and the outline each node gets.
struct Plan {
    graph: gl::Graph,
    shapes: Vec<BoardShape>,
    /// For each link, its edge in the graph.
    edges: Vec<Option<usize>>,
}

fn plan(chart: &mm::Flowchart, warnings: &mut Vec<String>) -> Plan {
    let mut group = vec![None; chart.nodes.len()];
    for (i, sg) in chart.subgraphs.iter().enumerate() {
        for m in &sg.members {
            if let Some(g) = group.get_mut(*m) {
                *g = Some(i);
            }
        }
        if sg.direction.is_some_and(|d| d != chart.direction) {
            warnings.push(format!("subgraph `{}`: its own direction isn't kept; it follows the flowchart's", sg.id));
        }
    }
    let mut shapes = Vec::with_capacity(chart.nodes.len());
    let mut nodes = Vec::with_capacity(chart.nodes.len());
    for (i, node) in chart.nodes.iter().enumerate() {
        let (shape, round) = outline(node.shape);
        if exact(node.shape).is_none() {
            warnings.push(format!("node `{}`: the {:?} shape is drawn as {}", node.id, node.shape, shape.label().to_lowercase()));
        }
        if node.markdown {
            warnings.push(format!("node `{}`: its label's markdown is shown as plain text", node.id));
        }
        shapes.push(shape);
        nodes.push(gl::NodeSpec { size: shape_size(shape, round, &node.label), group: group.get(i).copied().flatten() });
    }
    let groups =
        chart.subgraphs.iter().map(|sg| gl::GroupSpec { parent: sg.parent, padding: SECTION_PADDING, title_height: SECTION_TITLE }).collect();
    let mut graph = gl::Graph { nodes, edges: Vec::new(), groups };
    let mut edges = Vec::with_capacity(chart.edges.len());
    for edge in &chart.edges {
        let (Some(f), Some(t)) = (first_node(chart, edge.from), first_node(chart, edge.to)) else {
            edges.push(None);
            continue;
        };
        if f == t {
            edges.push(None);
            continue;
        }
        let mut e = gl::EdgeSpec::new(f, t);
        e.min_len = u32::from(edge.length.max(1));
        e.label = edge.label.as_deref().filter(|l| !l.trim().is_empty()).map(|l| {
            let (w, h) = measure(l, LABEL_SIZE);
            gl::Size::new(w + 8.0, h + 4.0)
        });
        edges.push(Some(graph.edges.len()));
        graph.edges.push(e);
    }
    Plan { graph, shapes, edges }
}

/// The node a link to `e` lays out against: the node, or a subgraph's first node (searching its
/// nested subgraphs when it has none of its own).
fn first_node(chart: &mm::Flowchart, e: mm::Endpoint) -> Option<usize> {
    match e {
        mm::Endpoint::Node(n) => (n < chart.nodes.len()).then_some(n),
        mm::Endpoint::Subgraph(g) => {
            let mut queue = vec![g];
            let mut seen = 0;
            while let Some(g) = queue.pop() {
                seen += 1;
                if seen > chart.subgraphs.len() {
                    return None;
                }
                let sg = chart.subgraphs.get(g)?;
                if let Some(m) = sg.members.first() {
                    return Some(*m);
                }
                queue.extend(chart.subgraphs.iter().enumerate().filter(|(_, c)| c.parent == Some(g)).map(|(i, _)| i).rev());
            }
            None
        }
    }
}

fn end_name(chart: &mm::Flowchart, e: mm::Endpoint) -> String {
    match e {
        mm::Endpoint::Node(n) => chart.nodes.get(n).map_or_else(String::new, |x| x.id.clone()),
        mm::Endpoint::Subgraph(g) => chart.subgraphs.get(g).map_or_else(String::new, |x| x.id.clone()),
    }
}

/// Subgraph indices with each one after the one around it.
fn by_depth(subgraphs: &[mm::Subgraph]) -> Vec<usize> {
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = subgraphs.get(i).and_then(|s| s.parent) {
            d += 1;
            if d > subgraphs.len() {
                break;
            }
            i = p;
        }
        d
    };
    let mut order: Vec<usize> = (0..subgraphs.len()).collect();
    order.sort_by_key(|i| depth(*i));
    order
}

/// The outline a Mermaid shape is drawn with, and whether it is round (as wide as it is tall).
fn outline(shape: mm::Shape) -> (BoardShape, bool) {
    use BoardShape as B;
    use mm::Shape as S;
    if let Some(b) = exact(shape) {
        return (b, matches!(shape, S::Circle | S::SmallCircle | S::DoubleCircle));
    }
    let b = match shape {
        S::FramedCircle => B::DoubleCircle,
        S::FilledCircle | S::CrossedCircle => B::Ellipse,
        S::LinedDocument | S::StackedDocument | S::TaggedDocument => B::Document,
        S::HorizontalCylinder | S::LinedCylinder | S::DataStore => B::Database,
        S::NotchedPentagon => B::Pentagon,
        S::CurvedTrapezoid => B::TrapezoidDown,
        S::Flag => B::Flag,
        S::Hourglass => B::TriangleDown,
        S::Cloud | S::Bang => B::Ellipse,
        _ => B::Rectangle,
    };
    (b, matches!(shape, S::FramedCircle | S::FilledCircle | S::CrossedCircle))
}

/// The outline that is the same as Mermaid shape `shape`, when there is one.
fn exact(shape: mm::Shape) -> Option<BoardShape> {
    use BoardShape as B;
    use mm::Shape as S;
    Some(match shape {
        S::Rect => B::Rectangle,
        S::Rounded => B::RoundedRectangle,
        S::Stadium => B::Stadium,
        S::Subroutine => B::PredefinedProcess,
        S::Cylinder => B::Database,
        S::Circle | S::SmallCircle => B::Ellipse,
        S::DoubleCircle => B::DoubleCircle,
        S::Diamond => B::Diamond,
        S::Hexagon => B::Hexagon,
        S::Asymmetric => B::Flag,
        S::LeanRight => B::ParallelogramRight,
        S::LeanLeft => B::ParallelogramLeft,
        S::TrapezoidBottom => B::Trapezoid,
        S::TrapezoidTop => B::TrapezoidDown,
        S::SlopedRect => B::ManualInput,
        S::NotchedRect => B::Card,
        S::WindowPane => B::InternalStorage,
        S::Delay => B::Delay,
        S::Document => B::Document,
        S::Triangle => B::Triangle,
        S::FlippedTriangle => B::TriangleDown,
        _ => return None,
    })
}

/// A shape's size: its label measured, with room around it and inside the outline's insets.
fn shape_size(shape: BoardShape, round: bool, label: &str) -> gl::Size {
    let (tw, th) = measure(label, TEXT_SIZE);
    let [t, r, b, l] = shape.text_insets();
    let w = ((tw + 2.0 * PAD.0) / (1.0 - l - r).max(0.2)).max(MIN_SIZE.0);
    let h = ((th + 2.0 * PAD.1) / (1.0 - t - b).max(0.2)).max(MIN_SIZE.1);
    let (w, h) = if round { (w.max(h), w.max(h)) } else { (w, h) };
    gl::Size::new(w.min(100_000.0).ceil(), h.min(100_000.0).ceil())
}

/// The size `text` takes at `size`, wrapped at [`MAX_TEXT_WIDTH`]. Without fonts to measure
/// with, an estimate from the characters.
fn measure(text: &str, size: f64) -> (f64, f64) {
    let mut td = TextData { characters: text.to_string(), ..TextData::default() };
    td.style.font_size = size;
    let l = figcraft_text::layout_cached(&td, None, None);
    let (w, h) = if l.width > MAX_TEXT_WIDTH {
        let wrapped = figcraft_text::layout_cached(&td, Some(MAX_TEXT_WIDTH), None);
        (MAX_TEXT_WIDTH, wrapped.height)
    } else {
        (l.width, l.height)
    };
    if w > 0.0 && h > 0.0 {
        return (w, h);
    }
    let lines = text.lines().count().max(1) as f64;
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f64;
    let w = (longest * size * 0.55).min(MAX_TEXT_WIDTH);
    let wraps = (longest * size * 0.55 / MAX_TEXT_WIDTH).ceil().max(1.0);
    (w, lines * wraps * size * 1.3)
}

/// The sides a connector leaves and arrives on: where the layout's line for edge `k` meets each
/// end's box.
fn sides(layout: &gl::Layout, graph: &gl::Graph, k: usize) -> (&'static str, &'static str) {
    let (Some(line), Some(e)) = (layout.edges.get(k), graph.edges.get(k)) else { return ("auto", "auto") };
    let side = |p: Option<&gl::Point>, n: usize| match (p, layout.nodes.get(n)) {
        (Some(p), Some(r)) => side_of(*p, r),
        _ => "auto",
    };
    (side(line.first(), e.from), side(line.last(), e.to))
}

fn side_of(p: gl::Point, r: &gl::Rect) -> &'static str {
    let d = [("top", (p.y - r.y).abs()), ("right", (p.x - r.right()).abs()), ("bottom", (p.y - r.bottom()).abs()), ("left", (p.x - r.x).abs())];
    d.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).map_or("auto", |(s, _)| s)
}

fn direction(d: mm::Direction) -> gl::Direction {
    match d {
        mm::Direction::TopToBottom => gl::Direction::Down,
        mm::Direction::BottomToTop => gl::Direction::Up,
        mm::Direction::LeftToRight => gl::Direction::Right,
        mm::Direction::RightToLeft => gl::Direction::Left,
    }
}

fn direction_of(v: &str) -> Option<gl::Direction> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "down" | "tb" | "td" => gl::Direction::Down,
        "up" | "bt" => gl::Direction::Up,
        "right" | "lr" => gl::Direction::Right,
        "left" | "rl" => gl::Direction::Left,
        _ => return None,
    })
}

fn cap(head: mm::Head, name: &dyn Fn() -> String, warnings: &mut Vec<String>) -> &'static str {
    match head {
        mm::Head::None => "none",
        mm::Head::Arrow => "triangleArrow",
        mm::Head::Circle => "circleFilled",
        mm::Head::Cross => {
            warnings.push(format!("{}: a cross end is drawn as a plain end", name()));
            "none"
        }
    }
}

/// A colour from a Mermaid style value; a warning when it can't be read.
fn colour(v: &Option<String>, what: &str, who: &str, warnings: &mut Vec<String>) -> Option<Color> {
    let v = v.as_deref()?;
    let c = Color::from_hex_or_name(v);
    if c.is_none() {
        warnings.push(format!("`{who}`: the {what} colour `{v}` can't be read, so it's left out"));
    }
    c
}

/// Outline stroke, its weight and dashes, and the label's colour from a node's style.
fn style_shape(doc: &mut Document, id: NodeId, style: &mm::Style, who: &str, warnings: &mut Vec<String>) {
    let stroke = colour(&style.stroke, "stroke", who, warnings);
    let ink = colour(&style.color, "text", who, warnings);
    let children = doc.node(id).map(|n| n.children.clone()).unwrap_or_default();
    for c in children {
        let Some(n) = doc.node_mut(c) else { continue };
        match n.kind {
            NodeKind::Vector { .. } => {
                if let Some(c) = stroke {
                    n.strokes = vec![Paint::solid(c)];
                }
                if let Some(w) = style.stroke_width.filter(|w| w.is_finite() && (0.0..=100.0).contains(w)) {
                    n.stroke.weight = w;
                }
                if let Some(d) = dashes(style) {
                    n.stroke.dashes = d;
                }
            }
            NodeKind::Text(_) => {
                if let Some(c) = ink {
                    n.fills = vec![Paint::solid(c)];
                }
            }
            _ => {}
        }
    }
    ignored(style, who, warnings);
}

/// A connector's dashes, weight and colour from its link's line and style.
fn style_line(doc: &mut Document, id: NodeId, stroke: mm::Stroke, style: &mm::Style, who: &str, warnings: &mut Vec<String>) {
    let colour = colour(&style.stroke, "stroke", who, warnings);
    let Some(n) = doc.node_mut(id) else { return };
    match stroke {
        mm::Stroke::Dotted => n.stroke.dashes = DOTTED.to_vec(),
        mm::Stroke::Thick => n.stroke.weight = THICK,
        mm::Stroke::Solid | mm::Stroke::Invisible => {}
    }
    if let Some(c) = colour {
        n.strokes = vec![Paint::solid(c)];
    }
    if let Some(w) = style.stroke_width.filter(|w| w.is_finite() && (0.0..=100.0).contains(w)) {
        n.stroke.weight = w;
    }
    if let Some(d) = dashes(style) {
        n.stroke.dashes = d;
    }
    if style.fill.is_some() || style.color.is_some() {
        warnings.push(format!("{who}: a link's fill and text colour are left out"));
    }
    ignored(style, who, warnings);
}

fn dashes(style: &mm::Style) -> Option<Vec<f64>> {
    let d = style.stroke_dasharray.as_ref()?;
    let ok = !d.is_empty() && d.len() <= 16 && d.iter().all(|v| v.is_finite() && (0.0..=1000.0).contains(v)) && d.iter().any(|v| *v > 0.0);
    ok.then(|| d.clone())
}

fn ignored(style: &mm::Style, who: &str, warnings: &mut Vec<String>) {
    if !style.other.is_empty() {
        let keys: Vec<&str> = style.other.iter().map(|(k, _)| k.as_str()).take(8).collect();
        warnings.push(format!("`{who}`: style {} left out", keys.join(", ")));
    }
}
