//! A Mermaid flowchart becomes shapes, sections and connectors laid out without overlaps, in one
//! undo step; `board.tidyDiagram` lays out shapes already on a board.

use figcraft_doc::{BoardItem, End, NodeId};
use figcraft_engine::Session;
use figcraft_geom::Rect;
use serde_json::{Value, json};

fn board() -> Session {
    let mut s = Session::new();
    s.execute("file.newBoard", &json!({"name": "Diagram"})).unwrap();
    s
}

fn ids(v: &Value) -> Vec<NodeId> {
    v["ids"].as_array().unwrap().iter().map(|i| NodeId(i.as_u64().unwrap())).collect()
}

/// Every board item on the current page, with what it is.
fn items(s: &Session) -> Vec<(NodeId, BoardItem)> {
    let doc = s.doc().unwrap();
    let mut out = Vec::new();
    let mut stack = doc.node(s.page().unwrap()).unwrap().children.clone();
    while let Some(id) = stack.pop() {
        let n = doc.node(id).unwrap();
        if let Some(b) = &n.board {
            out.push((id, b.clone()));
        }
        if n.board.is_none() {
            stack.extend(n.children.iter().copied());
        }
    }
    out
}

fn shapes(s: &Session) -> Vec<NodeId> {
    items(s).into_iter().filter(|(_, b)| matches!(b, BoardItem::Shape { .. })).map(|(id, _)| id).collect()
}

fn connectors(s: &Session) -> Vec<(NodeId, NodeId, NodeId)> {
    items(s)
        .into_iter()
        .filter_map(|(id, b)| match b {
            BoardItem::Connector(c) => Some((id, c.from.node()?, c.to.node()?)),
            _ => None,
        })
        .collect()
}

fn bounds(s: &Session, id: NodeId) -> Rect {
    s.doc().unwrap().world_bounds(id).unwrap()
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 - 0.5 && b.x0 < a.x1 - 0.5 && a.y0 < b.y1 - 0.5 && b.y0 < a.y1 - 0.5
}

fn no_overlaps(s: &Session, of: &[NodeId]) {
    for (i, a) in of.iter().enumerate() {
        for b in &of[i + 1..] {
            assert!(!overlap(bounds(s, *a), bounds(s, *b)), "{a:?} and {b:?} overlap");
        }
    }
}

/// The shape whose text is `text`.
fn shape_named(s: &Session, text: &str) -> NodeId {
    let doc = s.doc().unwrap();
    let says = |id: NodeId| match figcraft_engine::board::text_of(doc, id).and_then(|t| doc.node(t)).map(|t| &t.kind) {
        Some(figcraft_doc::NodeKind::Text(t)) => t.characters == text,
        _ => false,
    };
    shapes(s).into_iter().find(|id| says(*id)).unwrap_or_else(|| panic!("no shape says {text:?}"))
}

const CHART: &str = "flowchart LR
    start([Start]) --> check{Ready?}
    check -->|yes| build[Build it]
    check -.->|no| wait[(Queue)]
    wait ==> check
    subgraph ship [Shipping]
        build --> test[Test] --> done((Done))
    end
";

#[test]
fn a_flowchart_becomes_shapes_and_connectors_that_dont_overlap() {
    let mut s = board();
    let out = s.execute("board.importMermaid", &json!({"text": CHART, "x": 0, "y": 0})).unwrap();
    assert_eq!(out["shapes"], 6, "{out}");
    assert_eq!(out["connectors"], 6, "{out}");
    assert_eq!(out["sections"], 1, "{out}");
    assert_eq!(shapes(&s).len(), 6);
    assert_eq!(connectors(&s).len(), 6);
    no_overlaps(&s, &shapes(&s));
    // What it made is selected.
    assert_eq!(s.selection(), ids(&out));
    // Each link joins the shapes it names.
    let (check, build, wait) = (shape_named(&s, "Ready?"), shape_named(&s, "Build it"), shape_named(&s, "Queue"));
    let joins = connectors(&s).into_iter().map(|(_, f, t)| (f, t)).collect::<Vec<_>>();
    for pair in [(check, build), (check, wait), (wait, check)] {
        assert!(joins.contains(&pair), "no connector {pair:?} in {joins:?}");
    }
}

#[test]
fn left_to_right_runs_across_and_top_down_runs_down() {
    let mut s = board();
    s.execute("board.importMermaid", &json!({"text": "flowchart LR\n a --> b", "x": 0, "y": 0})).unwrap();
    let (a, b) = (bounds(&s, shape_named(&s, "a")), bounds(&s, shape_named(&s, "b")));
    assert!(b.x0 >= a.x1, "LR: b right of a: {a:?} {b:?}");
    let mut s = board();
    s.execute("board.importMermaid", &json!({"text": "graph TD\n a --> b", "x": 0, "y": 0})).unwrap();
    let (a, b) = (bounds(&s, shape_named(&s, "a")), bounds(&s, shape_named(&s, "b")));
    assert!(b.y0 >= a.y1, "TD: b below a: {a:?} {b:?}");
}

#[test]
fn subgraph_members_sit_inside_their_section() {
    let mut s = board();
    s.execute("board.importMermaid", &json!({"text": CHART})).unwrap();
    let doc = s.doc().unwrap();
    let sec = doc.node(s.page().unwrap()).unwrap().children.iter().copied().find(|c| doc.node(*c).unwrap().name == "Shipping").unwrap();
    let area = bounds(&s, sec);
    for text in ["Build it", "Test", "Done"] {
        let id = shape_named(&s, text);
        assert_eq!(doc.node(id).unwrap().parent, Some(sec), "{text} is in the section");
        let b = bounds(&s, id);
        assert!(b.x0 >= area.x0 && b.y0 >= area.y0 && b.x1 <= area.x1 && b.y1 <= area.y1, "{text} {b:?} inside {area:?}");
    }
    let outside = bounds(&s, shape_named(&s, "Start"));
    assert!(!overlap(outside, area), "a node outside the subgraph stays out of its section");
}

#[test]
fn a_longer_label_makes_a_wider_shape() {
    let mut s = board();
    s.execute("board.importMermaid", &json!({"text": "flowchart TD\n a[Go] --> b[A much longer label than the other one]"})).unwrap();
    assert!(bounds(&s, shape_named(&s, "A much longer label than the other one")).width() > bounds(&s, shape_named(&s, "Go")).width());
}

#[test]
fn one_undo_takes_the_whole_import_away() {
    let mut s = board();
    s.execute("board.importMermaid", &json!({"text": CHART})).unwrap();
    assert!(!items(&s).is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(items(&s).is_empty(), "left behind: {:?}", items(&s));
}

#[test]
fn another_kind_of_diagram_or_unreadable_text_adds_nothing() {
    let mut s = board();
    for text in ["sequenceDiagram\n Alice->>Bob: Hi", "pie title Pets\n \"Dogs\" : 3", "not a diagram at all", ""] {
        let e = s.execute("board.importMermaid", &json!({"text": text}));
        assert!(e.is_err(), "{text:?} was imported: {e:?}");
        assert!(items(&s).is_empty(), "{text:?} left {:?}", items(&s));
    }
    assert!(s.execute("board.importMermaid", &json!({"text": "sequenceDiagram\n A->>B: x"})).unwrap_err().to_string().contains("sequenceDiagram"));
}

#[test]
fn a_shape_without_an_outline_of_its_own_comes_with_a_warning() {
    let mut s = board();
    let out = s.execute("board.importMermaid", &json!({"text": "flowchart TD\n a@{ shape: cloud, label: \"Sky\" } --> b[Plain]"})).unwrap();
    let warnings = out["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("`a`")), "{out}");
    assert!(!warnings.iter().any(|w| w.as_str().unwrap().contains("`b`")), "{out}");
}

#[test]
fn hostile_flowcharts_are_refused_or_imported_without_a_panic() {
    let mut s = board();
    let many = (0..300).map(|i| format!("n{i} --> n{}", i + 1)).collect::<Vec<_>>().join("\n");
    let deep = (0..40).map(|i| format!("subgraph s{i}\n")).collect::<String>() + "x --> y\n" + &"end\n".repeat(40);
    let cases = [
        "flowchart TD\n a --> a".to_string(),
        "flowchart TD\n subgraph empty\n end\n a --> empty".to_string(),
        "flowchart TD\n a[\"\"] --> b[\"   \"]".to_string(),
        "flowchart TD\n a --> b\n style a fill:#zzz,stroke-width:1e308px,stroke-dasharray:-5 nan".to_string(),
        "flowchart TD\n a ~~~ b\n a e1@--x b\n b --o c".to_string(),
        format!("flowchart LR\n{many}"),
        format!("flowchart TD\n{deep}"),
        format!("flowchart TD\n a[{}] --> b", "W".repeat(5000)),
        "flowchart TD\n subgraph a\n subgraph b\n x\n end\n end\n a --> b".to_string(),
        "graph".to_string(),
        "flowchart\u{0}TD\n\u{202e}a --> b".to_string(),
    ];
    for text in cases {
        let _ = s.execute("board.importMermaid", &json!({"text": text}));
    }
}

#[test]
fn tidy_lays_overlapping_shapes_out_along_their_connectors() {
    let mut s = board();
    let shape = |s: &mut Session, x: f64| {
        let v = s.execute("board.shape", &json!({"shape": "rectangle", "x": x, "y": 40, "width": 120, "height": 80})).unwrap();
        NodeId(v["id"].as_u64().unwrap())
    };
    let [a, b, c] = [shape(&mut s, 0.0), shape(&mut s, 30.0), shape(&mut s, 60.0)];
    s.execute("board.connector", &json!({"from": a, "to": b, "style": "elbow"})).unwrap();
    s.execute("board.connector", &json!({"from": b, "to": c, "style": "elbow"})).unwrap();
    let corner = |s: &Session| [a, b, c].iter().map(|n| bounds(s, *n)).fold((f64::MAX, f64::MAX), |(x, y), r| (x.min(r.x0), y.min(r.y0)));
    let before = corner(&s);
    s.execute("board.tidyDiagram", &json!({"direction": "down"})).unwrap();
    no_overlaps(&s, &[a, b, c]);
    let after = corner(&s);
    assert!((before.0 - after.0).abs() < 1.0 && (before.1 - after.1).abs() < 1.0, "the top-left stays: {before:?} → {after:?}");
    assert!(bounds(&s, b).y0 >= bounds(&s, a).y1 && bounds(&s, c).y0 >= bounds(&s, b).y1, "down the connectors");
    // The connectors now leave from the bottom and arrive at the top.
    for (_, it) in items(&s) {
        if let BoardItem::Connector(k) = it {
            assert!(matches!(k.from, End::Node { side: figcraft_doc::Side::Bottom, .. }), "{k:?}");
            assert!(matches!(k.to, End::Node { side: figcraft_doc::Side::Top, .. }), "{k:?}");
        }
    }
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(overlap(bounds(&s, a), bounds(&s, b)), "one undo puts them back");
}

#[test]
fn tidy_needs_two_shapes() {
    let mut s = board();
    assert!(s.execute("board.tidyDiagram", &json!({})).is_err());
    assert!(s.execute("board.tidyDiagram", &json!({"direction": "sideways"})).is_err());
}
