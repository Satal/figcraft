//! Elbow connectors go around the objects in their way, and find their way again when one moves.

use figcraft_doc::{NodeId, NodeKind};
use figcraft_engine::Session;
use figcraft_geom::{Point, Rect};
use serde_json::{Value, json};

fn board() -> Session {
    let mut s = Session::new();
    s.execute("file.newBoard", &json!({"name": "Routes"})).unwrap();
    s
}

fn id(v: &Value) -> NodeId {
    NodeId(v["id"].as_u64().unwrap())
}

fn shape(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> NodeId {
    id(&s.execute("board.shape", &json!({"shape": "rectangle", "x": x, "y": y, "width": w, "height": h})).unwrap())
}

/// Connector `id`'s points on the page.
fn drawn(s: &Session, id: NodeId) -> Vec<Point> {
    let doc = s.doc().unwrap();
    let NodeKind::Vector { network } = &doc.node(id).unwrap().kind else { panic!("not a vector") };
    let w = doc.world_transform(id);
    network.vertices.iter().map(|v| w * v.point()).collect()
}

/// Whether the polyline runs through the inside of `r`.
fn crosses(pts: &[Point], r: Rect) -> bool {
    pts.windows(2).any(|s| {
        let (a, b) = (s[0], s[1]);
        a.x.min(b.x) < r.x1 - 0.5 && a.x.max(b.x) > r.x0 + 0.5 && a.y.min(b.y) < r.y1 - 0.5 && a.y.max(b.y) > r.y0 + 0.5
    })
}

fn bounds(s: &Session, id: NodeId) -> Rect {
    s.doc().unwrap().world_bounds(id).unwrap()
}

#[test]
fn an_elbow_goes_around_a_shape_in_its_way() {
    let mut s = board();
    let a = shape(&mut s, 0.0, 0.0, 120.0, 80.0);
    let c = shape(&mut s, 600.0, 0.0, 120.0, 80.0);
    let blocker = shape(&mut s, 260.0, -60.0, 120.0, 200.0);
    let line = id(&s.execute("board.connector", &json!({"from": a, "to": c, "style": "elbow"})).unwrap());
    let pts = drawn(&s, line);
    assert!(!crosses(&pts, bounds(&s, blocker)), "the elbow runs through the shape in its way: {pts:?}");
    // It still joins the two shapes: from beside the first to beside the second.
    let (first, last) = (pts[0], *pts.last().unwrap());
    assert!(bounds(&s, a).inflate(8.0, 8.0).contains(first) && !bounds(&s, a).contains(first), "{first:?}");
    assert!(bounds(&s, c).inflate(8.0, 8.0).contains(last) && !bounds(&s, c).contains(last), "{last:?}");
    assert!(pts.windows(2).all(|w| w[0].x == w[1].x || w[0].y == w[1].y), "not right-angled: {pts:?}");
}

#[test]
fn moving_a_shape_into_the_way_routes_the_elbow_around_it_and_moving_it_away_straightens_it() {
    let mut s = board();
    let a = shape(&mut s, 0.0, 0.0, 120.0, 80.0);
    let c = shape(&mut s, 600.0, 0.0, 120.0, 80.0);
    let blocker = shape(&mut s, 260.0, 600.0, 120.0, 200.0);
    let line = id(&s.execute("board.connector", &json!({"from": a, "to": c, "style": "elbow"})).unwrap());
    let straight = drawn(&s, line);
    assert_eq!(straight.len(), 2, "nothing in the way: one run");
    s.execute("node.set", &json!({"id": blocker, "props": {"x": 260, "y": -60}})).unwrap();
    let pts = drawn(&s, line);
    assert!(!crosses(&pts, bounds(&s, blocker)), "the elbow runs through the shape moved into its way: {pts:?}");
    s.execute("node.set", &json!({"id": blocker, "props": {"x": 260, "y": 600}})).unwrap();
    assert_eq!(drawn(&s, line), straight, "with the way clear again the elbow takes the short way");
    // Undo puts the detour back with the shape.
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!crosses(&drawn(&s, line), bounds(&s, blocker)));
}

#[test]
fn straight_connectors_and_hand_shaped_elbows_keep_their_way() {
    let mut s = board();
    let a = shape(&mut s, 0.0, 0.0, 120.0, 80.0);
    let c = shape(&mut s, 600.0, 0.0, 120.0, 80.0);
    shape(&mut s, 260.0, -60.0, 120.0, 200.0);
    let line = id(&s.execute("board.connector", &json!({"from": a, "to": c, "style": "straight"})).unwrap());
    assert_eq!(drawn(&s, line).len(), 2, "a straight connector goes straight");
}
