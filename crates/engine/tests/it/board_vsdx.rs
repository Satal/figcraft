//! A board page as a VSDX drawing and back: shapes, text and glued connectors survive, what a
//! drawing has no place for is reported, hostile files are refused; shapes from a shape library
//! land in the view in one undo step.

use base64::Engine as _;
use craft_vsdx as vx;
use figcraft_doc::{BoardItem, NodeId, NodeKind};
use figcraft_engine::Session;
use serde_json::{Value, json};

fn board() -> Session {
    let mut s = Session::new();
    s.execute("file.newBoard", &json!({"name": "Drawing"})).unwrap();
    s
}

/// Every board item on the current page, with what it is.
fn items(s: &Session) -> Vec<(NodeId, BoardItem)> {
    let doc = s.doc().unwrap();
    let mut out = Vec::new();
    let mut stack = doc.node(s.page().unwrap()).unwrap().children.clone();
    while let Some(id) = stack.pop() {
        let n = doc.node(id).unwrap();
        match &n.board {
            Some(b) => out.push((id, b.clone())),
            None => stack.extend(n.children.iter().copied()),
        }
    }
    out
}

fn shapes(s: &Session) -> Vec<NodeId> {
    items(s).into_iter().filter(|(_, b)| matches!(b, BoardItem::Shape { .. })).map(|(id, _)| id).collect()
}

/// Each connector's two attached layers (`None` for a loose end).
fn connectors(s: &Session) -> Vec<(Option<NodeId>, Option<NodeId>)> {
    items(s)
        .into_iter()
        .filter_map(|(_, b)| match b {
            BoardItem::Connector(c) => Some((c.from.node(), c.to.node())),
            _ => None,
        })
        .collect()
}

fn text(s: &Session, id: NodeId) -> String {
    let doc = s.doc().unwrap();
    match figcraft_engine::board::text_of(doc, id).and_then(|t| doc.node(t)).map(|t| &t.kind) {
        Some(NodeKind::Text(t)) => t.characters.clone(),
        _ => String::new(),
    }
}

fn texts(s: &Session) -> Vec<String> {
    let mut v: Vec<String> = shapes(s).into_iter().map(|id| text(s, id)).collect();
    v.sort();
    v
}

fn id(v: &Value) -> u64 {
    v["id"].as_u64().unwrap()
}

/// Three shapes in a row, the first two joined by an elbow connector and the last two by a
/// straight one.
fn diagram(s: &mut Session) {
    let mut ids = Vec::new();
    for (i, (shape, label)) in [("rectangle", "Start"), ("diamond", "Ready?"), ("ellipse", "Done")].into_iter().enumerate() {
        let v =
            s.execute("board.shape", &json!({"shape": shape, "x": i as f64 * 240.0, "y": 40, "width": 140, "height": 90, "text": label})).unwrap();
        ids.push(id(&v));
    }
    s.execute("board.connector", &json!({"from": ids[0], "to": ids[1], "style": "elbow", "fromSide": "right", "toSide": "left"})).unwrap();
    s.execute("board.connector", &json!({"from": ids[1], "to": ids[2], "style": "straight"})).unwrap();
}

fn export(s: &mut Session) -> (Vec<u8>, Value) {
    let v = s.execute("board.exportVsdx", &json!({})).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap();
    (bytes, v)
}

fn import(s: &mut Session, bytes: &[u8]) -> figcraft_engine::Result<Value> {
    s.execute("board.importVsdx", &json!({"data": base64::engine::general_purpose::STANDARD.encode(bytes), "name": "d.vsdx"}))
}

#[test]
fn a_round_trip_keeps_the_shapes_their_text_and_the_attached_connectors() {
    let mut s = board();
    diagram(&mut s);
    let (bytes, out) = export(&mut s);
    assert_eq!(out["shapes"], 3);
    assert_eq!(out["connectors"], 2);
    let mut back = board();
    let r = import(&mut back, &bytes).unwrap();
    assert_eq!(shapes(&back).len(), shapes(&s).len());
    assert_eq!(texts(&back), texts(&s));
    let lines = connectors(&back);
    assert_eq!(lines.len(), 2);
    let on_page = shapes(&back);
    for (from, to) in lines {
        assert!(from.is_some_and(|f| on_page.contains(&f)) && to.is_some_and(|t| on_page.contains(&t)), "both ends attached to shapes");
    }
    assert!(r["warnings"].as_array().unwrap().is_empty(), "nothing lost: {r}");
    // The outlines came back as the same board shapes.
    let kinds = |s: &Session| {
        let mut v: Vec<String> = items(s)
            .into_iter()
            .filter_map(|(_, b)| match b {
                BoardItem::Shape { shape } => Some(shape.id().to_string()),
                _ => None,
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(kinds(&back), kinds(&s));
}

#[test]
fn exported_geometry_lies_inside_each_box_and_on_the_page() {
    let mut s = board();
    diagram(&mut s);
    let (bytes, _) = export(&mut s);
    let (drawing, _) = vx::read_with(&bytes, &vx::Limits::DEFAULT).unwrap();
    let page = &drawing.pages[0];
    assert_eq!(page.shapes.len(), 3);
    for sh in &page.shapes {
        assert!(sh.x >= 0.0 && sh.y >= 0.0 && sh.x + sh.width <= page.width && sh.y + sh.height <= page.height, "{} on the page", sh.name);
        let mut points = 0;
        for g in &sh.geometry {
            for seg in &g.segments {
                let ps: Vec<(f64, f64)> = match *seg {
                    vx::Segment::MoveTo(p) | vx::Segment::LineTo(p) => vec![p],
                    vx::Segment::CubicTo { to, .. } => vec![to],
                    vx::Segment::ArcTo { to, via, .. } => vec![to, via],
                    vx::Segment::Ellipse { centre, a, b } => vec![centre, a, b],
                };
                for (x, y) in ps {
                    points += 1;
                    let e = 0.01;
                    assert!(x >= -e && y >= -e && x <= sh.width + e && y <= sh.height + e, "{} point {x},{y} outside its box", sh.name);
                }
            }
        }
        assert!(points > 0, "{} has an outline", sh.name);
    }
}

#[test]
fn a_drawing_written_with_glued_connectors_imports_attached() {
    let shape = |id, x: f64, geometry| vx::Shape {
        id,
        name: format!("Box {id}"),
        x,
        y: 20.0,
        width: 100.0,
        height: 60.0,
        geometry: vec![geometry],
        text: format!("Step {id}"),
        fill: Some(vx::Rgba { r: 255, g: 255, b: 255, a: 255 }),
        line: Some(vx::Rgba { r: 0, g: 0, b: 0, a: 255 }),
        line_weight: 1.0,
        ..vx::Shape::default()
    };
    // A rectangle and a five-pointed zigzag no board shape matches (it stays its own outline).
    let zigzag = vx::GeomPath::polygon(&[(0.0, 0.0), (30.0, 60.0), (50.0, 10.0), (70.0, 60.0), (100.0, 0.0), (100.0, 60.0), (0.0, 60.0)]);
    let page = vx::Page {
        name: "Page 1".into(),
        width: 400.0,
        height: 200.0,
        shapes: vec![shape(1, 20.0, vx::GeomPath::rect(100.0, 60.0)), shape(2, 240.0, zigzag)],
        connectors: vec![vx::Connector {
            id: 3,
            from: Some((1, vx::ConnectionPoint::Shape)),
            to: Some((2, vx::ConnectionPoint::Shape)),
            points: vec![(120.0, 50.0), (240.0, 50.0)],
            end_arrow: vx::ARROW,
            line: Some(vx::Rgba { r: 0, g: 0, b: 0, a: 255 }),
            line_weight: 1.0,
            label: "next".into(),
            ..vx::Connector::default()
        }],
    };
    let bytes = vx::write(&vx::Drawing { pages: vec![page] }).unwrap();
    let mut s = board();
    let r = import(&mut s, &bytes).unwrap();
    assert_eq!(r["shapes"], 2);
    assert_eq!(texts(&s), ["Step 1", "Step 2"]);
    let lines = connectors(&s);
    assert_eq!(lines.len(), 1);
    let (from, to) = lines[0];
    assert!(from.is_some() && to.is_some() && from != to, "glued at both ends");
    let custom = items(&s).into_iter().filter(|(_, b)| matches!(b, BoardItem::Shape { shape } if shape.id() == "custom")).count();
    assert_eq!(custom, 1, "the zigzag keeps its own outline");
    // One undo takes the whole drawing away.
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(items(&s).is_empty());
}

#[test]
fn what_cant_be_carried_across_is_reported() {
    let mut s = board();
    diagram(&mut s);
    s.execute("board.stamp", &json!({"x": 10, "y": 10, "stamp": "heart"})).unwrap();
    s.execute("board.ink", &json!({"points": [[0, 300], [80, 340], [160, 300]]})).unwrap();
    let (bytes, out) = export(&mut s);
    let warnings: Vec<&str> = out["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(warnings.iter().any(|w| w.contains("stamp")), "stamps reported: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("ink") || w.contains("marker")), "ink reported: {warnings:?}");
    // A drawing of several pages: only one is added, and the result says so.
    let (mut drawing, _) = vx::read_with(&bytes, &vx::Limits::DEFAULT).unwrap();
    let second = drawing.pages[0].clone();
    drawing.pages.push(second);
    let mut back = board();
    let r = import(&mut back, &vx::write(&drawing).unwrap()).unwrap();
    assert!(!r["warnings"].as_array().unwrap().is_empty(), "the other page is reported");
    assert!(back.message.is_some(), "and shown to the user");
}

#[test]
fn hostile_files_are_refused_and_add_nothing() {
    let (good, _) = export(&mut {
        let mut s = board();
        diagram(&mut s);
        s
    });
    let mut hostile: Vec<Vec<u8>> = vec![Vec::new(), b"PK\x03\x04".to_vec(), vec![0xff; 4096], b"<?xml version=\"1.0\"?><x/>".to_vec()];
    for cut in [10, good.len() / 3, good.len() / 2, good.len() - 1] {
        hostile.push(good[..cut].to_vec());
    }
    let mut flipped = good.clone();
    for i in (30..flipped.len()).step_by(97) {
        flipped[i] ^= 0x5a;
    }
    hostile.push(flipped);
    for bytes in &hostile {
        let mut s = board();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| import(&mut s, bytes).map(|_| ())));
        assert!(r.is_ok(), "no panic on {} bytes", bytes.len());
        if r.is_ok_and(|r| r.is_err()) {
            assert!(items(&s).is_empty(), "a refused file adds nothing");
        }
        let lib = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.execute("board.loadShapeLibrary", &json!({"data": base64::engine::general_purpose::STANDARD.encode(bytes), "name": "x.vssx"}))
                .map(|_| ())
        }));
        assert!(lib.is_ok(), "no panic loading {} bytes as a stencil", bytes.len());
    }
    // A design file isn't a board: nothing to export.
    let mut design = Session::new();
    design.execute("file.new", &json!({})).unwrap();
    assert!(design.execute("board.exportVsdx", &json!({})).is_err());
}

#[test]
fn a_library_shape_lands_inside_the_view_in_one_undo_step() {
    let mut s = board();
    let libs = s.execute("board.shapeLibraries", &json!({})).unwrap();
    let all = libs["libraries"].as_array().unwrap();
    assert!(all.len() >= 4, "the built-in libraries");
    for lib in all {
        for sh in lib["shapes"].as_array().unwrap() {
            assert!(!sh["path"].as_str().unwrap().is_empty(), "{} has an outline", sh["name"]);
        }
    }
    let (cx, cy) = (1234.0, -567.0);
    s.execute("view.set", &json!({"cx": cx, "cy": cy, "zoom": 1})).unwrap();
    let view = {
        let v = &s.ds().unwrap().view;
        (v.cx, v.cy)
    };
    let network = all.iter().find(|l| l["id"] == "network").unwrap();
    let name = network["shapes"][0]["name"].as_str().unwrap().to_string();
    let before = shapes(&s).len();
    let r = s.execute("board.insertLibraryShape", &json!({"library": "network", "name": name})).unwrap();
    assert_eq!(shapes(&s).len(), before + 1);
    let b = s.doc().unwrap().world_bounds(NodeId(id(&r))).unwrap();
    assert!(b.x0 < view.0 && view.0 < b.x1 && b.y0 < view.1 && view.1 < b.y1, "around the view's centre: {b:?} {view:?}");
    assert_eq!(s.selection(), vec![NodeId(id(&r))]);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(shapes(&s).len(), before, "one undo takes it away");
    assert!(s.execute("board.insertLibraryShape", &json!({"library": "network", "name": "no such shape"})).is_err());
}
