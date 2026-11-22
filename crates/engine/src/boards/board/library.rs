//! Shape libraries: the built-in ones (the flowchart shapes, and network, UML and arrow shapes
//! drawn here) and VSSX stencils loaded with `board.loadShapeLibrary`, whose paths are kept in
//! preferences. `board.insertLibraryShape` adds one of their shapes to the board.

use std::sync::LazyLock;

use craft_vsdx as vx;
use figcraft_color::{Color, Paint};
use figcraft_doc::{BoardShape, NodeKind};
use figcraft_geom::{Affine, BezPath, Point, Rect, Shape as _};
use serde_json::{Value, json};

use super::vsdx::{MAX_FILE, custom_outline, file_param, geom_to_bez, run};
use super::{Result, Session, bad, num, require, string};

/// The most VSSX stencils kept loaded.
const MAX_LIBRARIES: usize = 32;
/// The most shapes taken from one stencil.
const MAX_SHAPES: usize = 2_000;
/// An inserted shape keeps its own size while its longer side is in this range, and is scaled to
/// [`INSERT_SIZE`] otherwise.
const NATIVE_SIZES: std::ops::RangeInclusive<f64> = 48.0..=320.0;
const INSERT_SIZE: f64 = 160.0;

/// A shape in a library: its outline in its own `width` × `height` box.
#[derive(Clone, Debug)]
pub struct LibraryShape {
    /// What `board.insertLibraryShape` calls it.
    pub name: String,
    /// What it is called in the panel (English, translated there for the built-in libraries).
    pub label: String,
    pub width: f64,
    pub height: f64,
    pub path: BezPath,
    /// The board outline it is, when it is one (it is inserted as that shape).
    pub board: Option<BoardShape>,
    pub fill: Option<Color>,
    pub line: Option<Color>,
    /// A stencil master's PNG icon, if it has one.
    pub icon_png: Option<Vec<u8>>,
}

/// A shape library: built in, or a VSSX stencil read from `path`.
#[derive(Clone, Debug)]
pub struct ShapeLibrary {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
    pub built_in: bool,
    pub shapes: Vec<LibraryShape>,
    /// Why a stencil kept in preferences couldn't be read again.
    pub error: Option<String>,
}

/// SVG path data in a 100 × 100 box, drawn at `w` × `h`.
type Drawn = (&'static str, &'static str, f64, f64, &'static str);

const NETWORK: &[Drawn] = &[
    ("server", "Server", 80.0, 120.0, "M10 0H90V100H10Z M10 33H90 M10 66H90 M20 16H45 M20 50H45 M20 83H45"),
    ("desktop", "Desktop computer", 120.0, 100.0, "M0 0H100V70H0Z M40 70L35 90H65L60 70 M25 100H75V90H25Z"),
    ("laptop", "Laptop", 140.0, 90.0, "M12 0H88V75H12Z M0 85L12 75H88L100 85V100H0Z"),
    ("phone", "Mobile phone", 60.0, 110.0, "M10 0H90V100H10Z M10 12H90 M10 85H90 M42 92H58"),
    ("cloud", "Cloud", 160.0, 100.0, "M25 85A20 20 0 0 1 20 45A25 25 0 0 1 60 25A22 22 0 0 1 88 48A19 19 0 0 1 80 85Z"),
    ("router", "Router", 100.0, 100.0, "M0 50A50 50 0 1 0 100 50A50 50 0 1 0 0 50Z M20 50H80 M50 20V80 M70 40L80 50L70 60 M40 30L50 20L60 30"),
    ("switch", "Switch", 140.0, 70.0, "M0 0H100V100H0Z M15 35H85 M75 20L85 35L75 50 M85 65H15 M25 50L15 65L25 80"),
    (
        "firewall",
        "Firewall",
        120.0,
        100.0,
        "M0 0H100V100H0Z M0 25H100 M0 50H100 M0 75H100 M50 0V25 M25 25V50 M75 25V50 M50 50V75 M25 75V100 M75 75V100",
    ),
    ("globe", "Internet", 100.0, 100.0, "M0 50A50 50 0 1 0 100 50A50 50 0 1 0 0 50Z M0 50H100 M30 50A20 50 0 1 0 70 50A20 50 0 1 0 30 50Z"),
    ("storage", "Storage array", 120.0, 100.0, "M0 0H100V100H0Z M0 25H100 M0 50H100 M0 75H100 M80 12H90 M80 37H90 M80 62H90 M80 87H90"),
];

const UML: &[Drawn] = &[
    ("class", "Class", 160.0, 120.0, "M0 0H100V100H0Z M0 25H100 M0 60H100"),
    ("actor", "Actor", 60.0, 120.0, "M38 12A12 12 0 1 0 62 12A12 12 0 1 0 38 12Z M50 24V65 M15 38H85 M50 65L20 100 M50 65L80 100"),
    ("package", "Package", 160.0, 120.0, "M0 0H40V15H100V100H0Z M0 15H40"),
    ("note", "Note", 140.0, 100.0, "M0 0H80L100 20V100H0Z M80 0V20H100"),
    ("component", "Component", 160.0, 100.0, "M15 0H100V100H15Z M0 20H30V35H0Z M0 65H30V80H0Z"),
    ("interface", "Interface", 60.0, 60.0, "M0 50A50 50 0 1 0 100 50A50 50 0 1 0 0 50Z"),
    ("node", "Node", 160.0, 120.0, "M0 20L20 0H100V80L80 100H0Z M0 20H80V100 M80 20L100 0"),
    ("state", "State", 160.0, 80.0, "M20 0H80A20 20 0 0 1 100 20V80A20 20 0 0 1 80 100H20A20 20 0 0 1 0 80V20A20 20 0 0 1 20 0Z"),
    ("lifeline", "Lifeline", 120.0, 240.0, "M0 0H100V20H0Z M50 20V100"),
];

const ARROWS: &[Drawn] = &[
    ("arrowUp", "Arrow up", 100.0, 160.0, "M50 0L100 45H70V100H30V45H0Z"),
    ("arrowDown", "Arrow down", 100.0, 160.0, "M30 0H70V55H100L50 100L0 55H30Z"),
    ("doubleArrow", "Double arrow", 180.0, 90.0, "M0 50L25 0V30H75V0L100 50L75 100V70H25V100Z"),
    ("doubleArrowVertical", "Double arrow vertical", 90.0, 180.0, "M50 0L100 25H70V75H100L50 100L0 75H30V25H0Z"),
    ("notchedArrow", "Notched arrow", 160.0, 90.0, "M0 30H65V0L100 50L65 100V70H0L15 50Z"),
    ("pentagonArrow", "Pentagon arrow", 160.0, 90.0, "M0 0H70L100 50L70 100H0Z"),
    ("uTurn", "U-turn arrow", 120.0, 120.0, "M0 100V40A40 40 0 0 1 80 40V55H100L70 85L40 55H60V40A20 20 0 0 0 20 40V100Z"),
    (
        "quadArrow",
        "Four-way arrow",
        140.0,
        140.0,
        "M50 0L65 15H57V43H85V35L100 50L85 65V57H57V85H65L50 100L35 85H43V57H15V65L0 50L15 35V43H43V15H35Z",
    ),
];

fn drawn(list: &[Drawn]) -> Vec<LibraryShape> {
    list.iter()
        .filter_map(|(name, label, w, h, d)| {
            let path = BezPath::from_svg(d).ok()?;
            Some(LibraryShape {
                name: (*name).to_string(),
                label: (*label).to_string(),
                width: *w,
                height: *h,
                path: Affine::scale_non_uniform(w / 100.0, h / 100.0) * path,
                board: None,
                fill: None,
                line: None,
                icon_png: None,
            })
        })
        .collect()
}

static BUILT_IN: LazyLock<Vec<ShapeLibrary>> = LazyLock::new(|| {
    let flowchart = BoardShape::ALL
        .into_iter()
        .map(|b| {
            let (w, h) = (super::SHAPE, if matches!(b, BoardShape::Rectangle | BoardShape::Ellipse) { super::SHAPE } else { super::SHAPE * 0.75 });
            LibraryShape {
                name: b.id().to_string(),
                label: b.label().to_string(),
                width: w,
                height: h,
                path: b.path(w, h),
                board: Some(b),
                fill: None,
                line: None,
                icon_png: None,
            }
        })
        .collect();
    let lib = |id: &str, name: &str, shapes| ShapeLibrary { id: id.into(), name: name.into(), path: None, built_in: true, shapes, error: None };
    vec![
        lib("flowchart", "Flowchart", flowchart),
        lib("network", "Network", drawn(NETWORK)),
        lib("uml", "UML", drawn(UML)),
        lib("arrows", "Arrows", drawn(ARROWS)),
    ]
});

/// The built-in libraries' names and the labels of their shapes drawn here (the flowchart ones
/// are the board shapes'): English text the UI translates.
pub fn built_in_labels() -> Vec<&'static str> {
    let mut v = vec!["Flowchart", "Network", "UML", "Arrows"];
    v.extend(NETWORK.iter().chain(UML).chain(ARROWS).map(|d| d.1));
    v
}

/// The built-in shape libraries.
pub fn built_in() -> &'static [ShapeLibrary] {
    &BUILT_IN
}

/// Every shape library: the built-in ones, then the stencils loaded, those kept in preferences
/// read again first if they aren't yet.
pub fn libraries(s: &mut Session) -> Vec<ShapeLibrary> {
    sync(s);
    built_in().iter().chain(&s.shape_libraries).cloned().collect()
}

/// Reads the stencils kept in preferences that aren't loaded; one that can't be read is listed
/// with its error.
fn sync(s: &mut Session) {
    let paths: Vec<String> = s.prefs.shape_libraries.clone();
    for path in paths {
        if s.shape_libraries.iter().any(|l| l.path.as_deref() == Some(path.as_str())) || s.shape_libraries.len() >= MAX_LIBRARIES {
            continue;
        }
        let lib = match crate::file_access::read_capped(&path, MAX_FILE).map_err(|e| e.to_string()).and_then(|b| stencil(&b, &path)) {
            Ok((lib, _)) => lib,
            Err(e) => ShapeLibrary {
                id: unique_id(s, &stem(&path)),
                name: stem(&path),
                path: Some(path.clone()),
                built_in: false,
                shapes: Vec::new(),
                error: Some(e),
            },
        };
        let mut lib = lib;
        lib.id = unique_id(s, &lib.id);
        lib.path = Some(path);
        s.shape_libraries.push(lib);
    }
}

fn stem(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |(a, _)| a);
    if stem.trim().is_empty() { "Stencil".to_string() } else { stem.to_string() }
}

/// `base`, or `base-2`, `base-3`…: one no library has.
fn unique_id(s: &Session, base: &str) -> String {
    let taken = |id: &str| built_in().iter().chain(&s.shape_libraries).any(|l| l.id == id);
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).unwrap_or_else(|| base.to_string())
}

/// Stencil `bytes` (from file `name`) as a shape library, with what reading it reported.
fn stencil(bytes: &[u8], name: &str) -> std::result::Result<(ShapeLibrary, Vec<String>), String> {
    let (st, warns) = vx::read_stencil_with(bytes, &vx::Limits::DEFAULT).map_err(|e| format!("this isn't a VSSX stencil that can be read ({e})"))?;
    let mut warnings: Vec<String> = warns.iter().map(ToString::to_string).collect();
    if st.masters.len() > MAX_SHAPES {
        warnings.push(format!("the stencil has {} shapes: only the first {MAX_SHAPES} were taken", st.masters.len()));
    }
    let mut shapes: Vec<LibraryShape> = Vec::new();
    for m in st.masters.iter().take(MAX_SHAPES) {
        match master(m) {
            Some(sh) => shapes.push(sh),
            None => warnings.push(format!("shape `{}`: it has no outline that can be drawn, so it was left out", m.name)),
        }
    }
    if shapes.is_empty() {
        return Err("the stencil has no shapes that can be drawn".to_string());
    }
    let stem = stem(name);
    Ok((ShapeLibrary { id: stem.clone(), name: stem, path: None, built_in: false, shapes, error: None }, warnings))
}

/// A stencil master as a library shape: its sub-shapes' outlines in one path in its box.
fn master(m: &vx::Master) -> Option<LibraryShape> {
    let mut path = BezPath::new();
    let (mut fill, mut line) = (None, None);
    for sh in &m.shapes {
        let Some(p) = geom_to_bez(&sh.geometry) else { continue };
        let (w, h) = (sh.width, sh.height);
        let turn = if sh.angle.is_finite() { sh.angle.to_radians() } else { 0.0 };
        let t = Affine::translate((sh.x + w / 2.0, sh.y + h / 2.0)) * Affine::rotate(turn) * Affine::translate((-w / 2.0, -h / 2.0));
        path.extend(t * p);
        fill = fill.or(sh.fill);
        line = line.or(sh.line);
    }
    if path.elements().is_empty() {
        return None;
    }
    let b = path.bounding_box();
    let ok = |v: f64| (1.0..=100_000.0).contains(&v);
    let (w, h) = if ok(m.width) && ok(m.height) { (m.width, m.height) } else { (b.width().max(1.0), b.height().max(1.0)) };
    if !(ok(w) && ok(h) && b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite()) {
        return None;
    }
    let label = if m.local_name.trim().is_empty() { m.name.clone() } else { m.local_name.clone() };
    let colour = |c: vx::Rgba| Color::from_rgba8(c.r, c.g, c.b, c.a);
    Some(LibraryShape {
        name: m.name.clone(),
        label,
        width: w,
        height: h,
        path,
        board: None,
        fill: fill.map(colour),
        line: line.map(colour),
        icon_png: m.icon_png.clone(),
    })
}

fn library_json(l: &ShapeLibrary) -> Value {
    let shapes: Vec<Value> = l
        .shapes
        .iter()
        .map(|sh| {
            let mut v = json!({"name": sh.name, "label": sh.label, "width": sh.width, "height": sh.height, "path": sh.path.to_svg()});
            if sh.icon_png.is_some() {
                v["hasIcon"] = json!(true);
            }
            v
        })
        .collect();
    let mut v = json!({"id": l.id, "name": l.name, "builtIn": l.built_in, "shapes": shapes});
    if let Some(p) = &l.path {
        v["path"] = json!(p);
    }
    if let Some(e) = &l.error {
        v["error"] = json!(e);
    }
    v
}

pub(super) fn list_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    let libs: Vec<Value> = libraries(s).iter().map(library_json).collect();
    Ok(json!({"libraries": libs}))
}

pub(super) fn load_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.loadShapeLibrary";
    let path = string(p, "path");
    let (bytes, name) = file_param(cmd, p, "stencil.vssx")?;
    let (mut lib, mut warnings) = stencil(&bytes, &name).map_err(|e| bad(cmd, format!("{e}: no shape library was added")))?;
    // Loading a stencil again replaces it.
    if let Some(path) = &path {
        s.shape_libraries.retain(|l| l.path.as_deref() != Some(path.as_str()));
    }
    if s.shape_libraries.len() >= MAX_LIBRARIES {
        return Err(bad(cmd, format!("{MAX_LIBRARIES} shape libraries are loaded already: remove one first")));
    }
    lib.id = unique_id(s, &lib.id);
    lib.path.clone_from(&path);
    match &path {
        Some(path) => {
            if !s.prefs.shape_libraries.contains(path) {
                s.prefs.shape_libraries.push(path.clone());
                s.prefs_unsaved = true;
            }
        }
        None => warnings.push("it was given as data, not a path, so it isn't remembered after FigCraft closes".to_string()),
    }
    let out = json!({"id": lib.id, "name": lib.name, "shapes": lib.shapes.len(), "warnings": warnings});
    s.shape_libraries.push(lib);
    Ok(out)
}

pub(super) fn remove_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.removeShapeLibrary";
    let id = string(p, "library").ok_or_else(|| bad(cmd, "`library` is required"))?;
    if built_in().iter().any(|l| l.id == id) {
        return Err(bad(cmd, format!("`{id}` is built in and can't be removed")));
    }
    let Some(i) = s.shape_libraries.iter().position(|l| l.id == id) else {
        return Err(bad(cmd, format!("no loaded shape library is called `{id}`")));
    };
    let lib = s.shape_libraries.remove(i);
    if let Some(path) = lib.path {
        // Not read again on the next listing.
        s.prefs.shape_libraries.retain(|p| *p != path);
        s.prefs_unsaved = true;
    }
    Ok(json!({"removed": id}))
}

pub(super) fn insert_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.insertLibraryShape";
    require(s, cmd)?;
    let lib_id = string(p, "library").ok_or_else(|| bad(cmd, "`library` is required"))?;
    let name = string(p, "name").ok_or_else(|| bad(cmd, "`name` is required"))?;
    sync(s);
    let shape = {
        let Some(lib) = built_in().iter().chain(&s.shape_libraries).find(|l| l.id == lib_id) else {
            return Err(bad(cmd, format!("no shape library is called `{lib_id}`")));
        };
        let Some(sh) = lib.shapes.iter().find(|sh| sh.name == name) else {
            return Err(bad(cmd, format!("shape library `{}` has no shape called `{name}`", lib.name)));
        };
        sh.clone()
    };
    let centre = match (num(p, "x"), num(p, "y")) {
        (None, None) => {
            let v = &s.ds()?.view;
            Point::new(v.cx, v.cy)
        }
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Point::new(x, y),
        _ => return Err(bad(cmd, "`x` and `y` are finite numbers, given together")),
    };
    let long = shape.width.max(shape.height);
    let k = if NATIVE_SIZES.contains(&long) { 1.0 } else { INSERT_SIZE / long.max(1e-9) };
    let (w, h) = ((shape.width * k).max(1.0), (shape.height * k).max(1.0));
    let mut sp = json!({
        "shape": shape.board.unwrap_or(BoardShape::Rectangle).id(),
        "width": w, "height": h,
        "x": centre.x - w / 2.0, "y": centre.y - h / 2.0,
    });
    if let Some(c) = shape.fill {
        sp["color"] = json!(c.to_hex());
    }
    let id = run(s, "board.shape", &sp)?;
    if shape.board.is_none() {
        let doc = s.doc_mut()?;
        let path = Affine::scale_non_uniform(w / shape.width.max(1e-9), h / shape.height.max(1e-9)) * shape.path.clone();
        let path = if path.elements().is_empty() { Rect::new(0.0, 0.0, w, h).to_path(0.1) } else { path };
        custom_outline(doc, id, &path, Some(&shape.label))?;
        if let Some(line) = shape.line {
            let outline = doc.get(id)?.children.iter().copied().find(|c| doc.node(*c).is_some_and(|k| matches!(k.kind, NodeKind::Vector { .. })));
            if let Some(n) = outline.and_then(|o| doc.node_mut(o)) {
                n.strokes = vec![Paint::solid(line)];
            }
        }
    }
    s.set_selection(vec![id]);
    Ok(json!({"id": id}))
}
