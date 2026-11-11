//! Ripple deletes across every track: the In/Out range, transcript words, and the ranges of
//! duration markers. Markers ripple with the cut whatever Ripple Sequence Markers says, and the
//! markers inside a removed range go with it, so a marked range can be cut in one step and the
//! markers after it still point at the same frames.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use filmcraft_edit::transcript as tx;
use filmcraft_project::{Marker, MarkerId};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, bad, has_seq, marker_color_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal: true, named: crate::commands::NamedKeys::NONE }
}

fn has_in_out(s: &Session) -> std::result::Result<(), String> {
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.mark_in.is_some() || q.mark_out.is_some() { Ok(()) } else { Err("mark an In or Out point first".into()) }
}

fn has_markers(s: &Session) -> std::result::Result<(), String> {
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.markers.iter().any(|m| m.duration > Tick::ZERO) { Ok(()) } else { Err("the sequence has no range markers".into()) }
}

/// Ripple-delete `ranges` on every unlocked track, ripple the markers, clear In/Out.
pub(crate) fn cut(s: &mut Session, label: &str, ranges: Vec<TimeRange>) -> Result<Value> {
    let ranges: Vec<TimeRange> = tx::merge_ranges(ranges).into_iter().filter(|r| r.duration > Tick::ZERO).collect();
    let Some(first) = ranges.first().map(|r| r.start) else {
        return Ok(json!({"removed": 0, "ticks": 0, "seconds": 0.0}));
    };
    let n = ranges.len();
    let total = s.edit_sequence(label, |q, ctx, _| {
        let total = tx::ripple_delete_ranges(q, ranges.clone(), ctx);
        for r in ranges.iter().rev() {
            crate::sequence_tools::ripple_markers(&mut q.markers, r.end(), -r.duration);
        }
        q.mark_in = None;
        q.mark_out = None;
        q.split = Default::default();
        Ok(total)
    })?;
    s.set_playhead(first);
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64}))
}

fn in_out(s: &mut Session, _: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + fd).unwrap_or(q.duration());
    if b <= a {
        return Err(EngineError::Other("In point is after Out point".into()));
    }
    cut(s, "Ripple Delete In to Out", vec![TimeRange::from_bounds(a, b)])
}

fn words(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transcript.rippleDelete";
    let words = tx::sequence_words(s.active_sequence().ok_or(EngineError::NoSequence)?, &s.project.transcripts);
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) is required"))? as usize;
    let to = u64_p(p, "to").map(|n| n as usize).unwrap_or(from);
    let r = tx::word_range(&words, from, to, s.sequence_rate())
        .ok_or_else(|| bad(cmd, format!("word index out of range (the transcript has {} words)", words.len())))?;
    cut(s, "Ripple Delete Text", vec![r])
}

/// The range markers `p` names: `marker`, `markers`, or every range marker matching `author` and/or `color`.
fn picked(s: &Session, p: &Value, cmd: &str) -> Result<Vec<Marker>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Option<BTreeSet<MarkerId>> = match (p.get("marker"), p.get("markers")) {
        (Some(_), Some(_)) => return Err(bad(cmd, "use marker or markers, not both")),
        (Some(v), None) => Some(BTreeSet::from([MarkerId(v.as_u64().ok_or_else(|| bad(cmd, "invalid marker id"))?)])),
        (None, Some(v)) => {
            let a = v.as_array().filter(|a| !a.is_empty()).ok_or_else(|| bad(cmd, "markers must be a list of ids"))?;
            Some(a.iter().map(|v| v.as_u64().map(MarkerId).ok_or_else(|| bad(cmd, "invalid marker id"))).collect::<Result<_>>()?)
        }
        (None, None) => None,
    };
    let author = str_p(p, "author");
    let color = marker_color_p(p, cmd)?;
    if ids.is_none() && author.is_none() && color.is_none() {
        return Err(bad(cmd, "name a marker, markers, author, or color"));
    }
    if let Some(ids) = &ids
        && let Some(missing) = ids.iter().find(|id| !q.markers.iter().any(|m| m.id == **id))
    {
        return Err(bad(cmd, format!("no marker {} in this sequence", missing.0)));
    }
    let found: Vec<Marker> = q
        .markers
        .iter()
        .filter(|m| ids.as_ref().is_none_or(|ids| ids.contains(&m.id)))
        .filter(|m| ids.is_some() || m.duration > Tick::ZERO)
        .filter(|m| author.is_none_or(|a| m.author == a))
        .filter(|m| match &color {
            None => true,
            Some((_, Some(hex))) => m.custom_color == Some(*hex),
            Some((label, None)) => m.custom_color.is_none() && m.color == *label,
        })
        .cloned()
        .collect();
    if found.is_empty() {
        return Err(bad(cmd, "no marker matches"));
    }
    if let Some(m) = found.iter().find(|m| m.duration <= Tick::ZERO) {
        return Err(bad(cmd, format!("marker {} has no duration", m.id.0)));
    }
    Ok(found)
}

fn select_marker(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "markers.selectRange";
    if p.get("markers").is_some() {
        return Err(bad(cmd, "pass one marker"));
    }
    let m = picked(s, p, cmd)?.remove(0);
    let fd = s.sequence_rate().frame_duration();
    let out = (m.start + m.duration - fd).max(m.start);
    s.edit_sequence("Mark Marker Range", |q, _, _| {
        q.mark_in = Some(m.start);
        q.mark_out = Some(out);
        Ok(())
    })?;
    s.set_playhead(m.start);
    Ok(json!({"marker": m.id.0, "start": m.start.0, "end": (m.start + m.duration).0}))
}

fn delete_markers(s: &mut Session, p: &Value) -> Result<Value> {
    let found = picked(s, p, "markers.rippleDelete")?;
    let ranges = found.iter().map(|m| TimeRange::new(m.start, m.duration)).collect();
    let mut v = cut(s, if found.len() == 1 { "Ripple Delete Marker Range" } else { "Ripple Delete Marker Ranges" }, ranges)?;
    v["markers"] = json!(found.iter().map(|m| m.id.0).collect::<Vec<_>>());
    Ok(v)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("sequence.rippleDeleteInOut", "Ripple Delete In to Out", &["Sequence"], "{} (every unlocked track)", has_in_out, in_out),
        spec("transcript.rippleDelete", "Ripple Delete Selected Text", &[], r#"{"from":word,"to":word?} (every unlocked track)"#, has_seq, words),
        spec("markers.selectRange", "Mark Marker Range", &[], r#"{"marker":id}"#, has_markers, select_marker),
        spec(
            "markers.rippleDelete",
            "Ripple Delete Marker Ranges",
            &[],
            r#"{"marker":id|"markers":[id]|"author":str?,"color":label_or_hex?} (every unlocked track; the markers go with their ranges)"#,
            has_markers,
            delete_markers,
        ),
    ]
}
