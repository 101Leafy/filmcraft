use filmcraft_engine::Session;
use filmcraft_time::Tick;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn ends(s: &Session) -> Vec<Tick> {
    let q = s.active_sequence().unwrap();
    q.video_tracks.iter().chain(q.audio_tracks.iter()).map(|t| t.items.iter().map(|c| c.start + c.duration).max().unwrap_or(Tick::ZERO)).collect()
}

fn frames(s: &Session, n: i64) -> Tick {
    s.sequence_rate().tick_of(n)
}

#[test]
fn in_out_ripples_every_track() {
    let mut s = demo();
    let before = ends(&s);
    s.execute("markers.markIn", json!({"frame":10})).unwrap();
    s.execute("markers.markOut", json!({"frame":19})).unwrap();
    let r = s.execute("sequence.rippleDeleteInOut", json!({})).unwrap();
    assert_eq!(r["ticks"].as_i64().unwrap(), frames(&s, 10).0);
    let cut = frames(&s, 10);
    for (a, b) in before.iter().zip(ends(&s)) {
        if *a > frames(&s, 20) {
            assert_eq!(b, *a - cut);
        }
    }
    let q = s.active_sequence().unwrap();
    assert_eq!((q.mark_in, q.mark_out), (None, None));
    assert_eq!(s.playhead(), frames(&s, 10));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(ends(&s), before);
}

#[test]
fn marker_range_selects_and_ripple_deletes() {
    let mut s = demo();
    let a = s.execute("markers.addRange", json!({"frame":10,"durationFrames":5,"color":"red"})).unwrap()["marker"].as_u64().unwrap();
    let b = s.execute("markers.addRange", json!({"frame":40,"durationFrames":5,"color":"red"})).unwrap()["marker"].as_u64().unwrap();
    let c = s.execute("markers.addRange", json!({"frame":60,"durationFrames":5,"color":"blue"})).unwrap()["marker"].as_u64().unwrap();
    s.execute("markers.selectRange", json!({"marker":a})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.mark_in, q.mark_out), (Some(frames(&s, 10)), Some(frames(&s, 14))));

    let before = ends(&s);
    let count = s.active_sequence().unwrap().markers.len();
    let r = s.execute("markers.rippleDelete", json!({"color":"red"})).unwrap();
    assert_eq!(r["removed"], 2);
    assert_eq!(r["markers"], json!([a, b]));
    for (x, y) in before.iter().zip(ends(&s)) {
        if *x > frames(&s, 45) {
            assert_eq!(y, *x - frames(&s, 10));
        }
    }
    let q = s.active_sequence().unwrap();
    assert_eq!(q.markers.len(), count - 2);
    assert_eq!(q.markers.iter().find(|m| m.id.0 == c).unwrap().start, frames(&s, 50));
}

#[test]
fn bad_marker_requests_fail_without_editing() {
    let mut s = demo();
    let count = s.active_sequence().unwrap().markers.len();
    let point = s.execute("markers.add", json!({"frame":10})).unwrap()["marker"].as_u64().unwrap();
    assert!(s.execute("markers.rippleDelete", json!({"marker":point})).is_err());
    assert!(s.execute("markers.rippleDelete", json!({})).is_err());
    assert!(s.execute("markers.rippleDelete", json!({"marker":999999})).is_err());
    assert!(s.execute("transcript.rippleDelete", json!({"from":0})).is_err());
    assert_eq!(s.active_sequence().unwrap().markers.len(), count + 1);
}
