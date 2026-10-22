use std::collections::VecDeque;

use serde_json::{json, Value};

use super::envelope::EventEnvelope;

pub const MAX_BUFFER_EVENTS: usize = 200;
pub const MAX_BUFFER_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Event,
    Ready,
    Gap,
    Rearmed,
    Ended,
}

struct Entry {
    start_seq: u64,
    end_seq: u64,
    kind: EntryKind,
    event: EventEnvelope,
    bytes: usize,
}

pub struct EventBuffer {
    subscription_id: String,
    entries: VecDeque<Entry>,
    next_seq: u64,
    consumed: u64,
    event_count: usize,
    event_bytes: usize,
    ready_cursor: Option<String>,
    ended_cursor: Option<String>,
}

impl EventBuffer {
    pub fn new() -> Self {
        Self {
            subscription_id: uuid::Uuid::new_v4().simple().to_string(),
            entries: VecDeque::new(),
            next_seq: 1,
            consumed: 0,
            event_count: 0,
            event_bytes: 0,
            ready_cursor: None,
            ended_cursor: None,
        }
    }

    pub fn append(&mut self, mut event: EventEnvelope) -> String {
        let kind = match event.meta.name.as_str() {
            "subscription.ready" => EntryKind::Ready,
            "subscription.gap" => EntryKind::Gap,
            "subscription.rearmed" => EntryKind::Rearmed,
            "subscription.ended" => EntryKind::Ended,
            _ => EntryKind::Event,
        };
        if kind == EntryKind::Ready {
            if let Some(cursor) = &self.ready_cursor {
                return cursor.clone();
            }
        }
        if kind == EntryKind::Ended {
            if let Some(cursor) = &self.ended_cursor {
                return cursor.clone();
            }
        }
        if kind == EntryKind::Gap {
            if let Some(last) = self.entries.back_mut().filter(|e| e.kind == EntryKind::Gap) {
                let missed = gap_missed(&last.event) + gap_missed(&event);
                last.end_seq = self.next_seq;
                self.next_seq += 1;
                last.event.data = json!({"missed": missed});
                last.event.meta.cursor = format!("{}:{}", self.subscription_id, last.end_seq);
                return last.event.meta.cursor.clone();
            }
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        let cursor = self.cursor(seq);
        event.meta.cursor = cursor.clone();
        event.meta.subscription_id = self.subscription_id.clone();
        let bytes = serialized_size(&event);
        if kind == EntryKind::Event || kind == EntryKind::Rearmed {
            self.event_count += 1;
            self.event_bytes += bytes;
        }
        if kind == EntryKind::Ready {
            self.ready_cursor = Some(cursor.clone());
        }
        if kind == EntryKind::Ended {
            self.ended_cursor = Some(cursor.clone());
        }
        self.entries.push_back(Entry {
            start_seq: seq,
            end_seq: seq,
            kind,
            event,
            bytes,
        });
        self.enforce_bounds();
        cursor
    }

    pub fn read(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, CursorError> {
        let since = match since {
            Some(cursor) => self.parse_cursor(cursor)?,
            None => 0,
        };
        let mut events = Vec::new();
        for entry in &self.entries {
            if entry.end_seq <= since {
                continue;
            }
            if events.len() == limit {
                break;
            }
            let mut event = entry.event.clone();
            if entry.kind == EntryKind::Gap {
                let missed = entry
                    .event
                    .data
                    .get("missed")
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
                let already_missed = since.saturating_sub(entry.start_seq.saturating_sub(1));
                event.data = json!({"missed": missed.saturating_sub(already_missed)});
            }
            events.push(event);
        }
        Ok(events)
    }

    pub fn ack(&mut self, seq: u64) {
        self.consumed = self.consumed.max(seq.min(self.head()));
    }

    pub fn start_cursor(&self) -> String {
        self.cursor(0)
    }

    pub fn head(&self) -> u64 {
        self.next_seq - 1
    }

    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    pub fn bytes(&self) -> usize {
        self.event_bytes
    }

    pub fn event_count(&self) -> usize {
        self.event_count
    }

    fn cursor(&self, seq: u64) -> String {
        format!("{}:{seq}", self.subscription_id)
    }

    fn parse_cursor(&self, cursor: &str) -> Result<u64, CursorError> {
        let Some((subscription_id, seq)) = cursor.rsplit_once(':') else {
            return Err(CursorError::CursorInvalid);
        };
        let seq = seq.parse::<u64>().map_err(|_| CursorError::CursorInvalid)?;
        if subscription_id != self.subscription_id || seq > self.head() {
            return Err(CursorError::CursorInvalid);
        }
        Ok(seq)
    }

    fn enforce_bounds(&mut self) {
        while self.event_count > MAX_BUFFER_EVENTS || self.event_bytes > MAX_BUFFER_BYTES {
            let Some(index) = self.entries.iter().position(|entry| {
                entry.kind == EntryKind::Event || entry.kind == EntryKind::Rearmed
            }) else {
                break;
            };
            let removed = self
                .entries
                .remove(index)
                .expect("entry index came from the buffer");
            self.event_count -= 1;
            self.event_bytes -= removed.bytes;
            self.insert_gap(index, removed);
        }
    }

    fn insert_gap(&mut self, mut index: usize, removed: Entry) {
        let mut start_seq = removed.start_seq;
        let mut end_seq = removed.end_seq;
        let mut missed = 1;
        let mut source = removed.event;

        if index > 0 && self.entries[index - 1].kind == EntryKind::Gap {
            let previous = self.entries.remove(index - 1).expect("gap index exists");
            index -= 1;
            start_seq = previous.start_seq;
            missed += gap_missed(&previous.event);
            source = previous.event;
        }
        if index < self.entries.len() && self.entries[index].kind == EntryKind::Gap {
            let next = self.entries.remove(index).expect("gap index exists");
            end_seq = next.end_seq;
            missed += gap_missed(&next.event);
        }

        let mut event = marker_from(&source, "subscription.gap", json!({"missed": missed}));
        event.meta.cursor = self.cursor(end_seq);
        self.entries.insert(
            index,
            Entry {
                start_seq,
                end_seq,
                kind: EntryKind::Gap,
                event,
                bytes: 0,
            },
        );
    }
}

impl Default for EventBuffer {
    fn default() -> Self {
        Self::new()
    }
}

fn gap_missed(event: &EventEnvelope) -> u64 {
    event
        .data
        .get("missed")
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

fn marker_from(source: &EventEnvelope, name: &str, data: Value) -> EventEnvelope {
    let mut marker = EventEnvelope::new(
        name,
        &source.meta.session,
        &source.meta.tab.id,
        &source.meta.tab.label,
        &source.meta.tab.target_id,
        data,
    );
    marker.meta.subscription_id = source.meta.subscription_id.clone();
    marker.meta.cursor = source.meta.cursor.clone();
    marker
}

fn serialized_size(event: &EventEnvelope) -> usize {
    serde_json::to_vec(event)
        .expect("event envelopes always serialize")
        .len()
}

#[derive(Debug, PartialEq, Eq)]
pub enum CursorError {
    CursorInvalid,
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CursorInvalid => f.write_str("cursor_invalid"),
        }
    }
}

impl std::error::Error for CursorError {}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use std::thread;

    use serde_json::{json, Value};

    use super::{CursorError, EventBuffer, MAX_BUFFER_BYTES, MAX_BUFFER_EVENTS};
    use crate::native::events::envelope::EventEnvelope;

    fn append(buffer: &mut EventBuffer, name: &str, data: Value) -> String {
        buffer.append(EventEnvelope::new(
            name, "session", "tab", "Tab", "target", data,
        ))
    }

    fn read_names(buffer: &EventBuffer, since: &str) -> Vec<String> {
        buffer
            .read(Some(since), usize::MAX)
            .unwrap()
            .into_iter()
            .map(|event| event.meta.name)
            .collect()
    }

    fn cursor_seq(event: &EventEnvelope) -> u64 {
        event
            .meta
            .cursor
            .rsplit_once(':')
            .unwrap()
            .1
            .parse()
            .unwrap()
    }

    #[test]
    fn reads_events_after_a_cursor() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        append(&mut buffer, "first", json!({}));
        append(&mut buffer, "second", json!({}));
        let third = append(&mut buffer, "third", json!({}));

        assert_eq!(buffer.read(Some(&start), 10).unwrap().len(), 3);
        assert!(buffer.read(Some(&third), 10).unwrap().is_empty());
    }

    #[test]
    fn reports_a_gap_for_an_old_cursor_then_continues_from_the_tail() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        for index in 0..MAX_BUFFER_EVENTS + 2 {
            append(&mut buffer, "change", json!({"index": index}));
        }

        let events = buffer.read(Some(&start), 10).unwrap();
        assert_eq!(events[0].meta.name, "subscription.gap");
        assert_eq!(events[0].data, json!({"missed": 2}));
        assert_eq!(events[1].data, json!({"index": 2}));
        assert_eq!(
            buffer.read(Some(&events[0].meta.cursor), 10).unwrap()[0].data,
            json!({"index": 2})
        );
    }

    #[test]
    fn rejects_foreign_and_future_cursors() {
        let mut buffer = EventBuffer::new();
        let other = EventBuffer::new();
        assert_eq!(
            buffer.read(Some(&other.start_cursor()), 1),
            Err(CursorError::CursorInvalid)
        );
        assert_eq!(
            buffer.read(
                Some(&format!(
                    "{}:1",
                    buffer.start_cursor().split(':').next().unwrap()
                )),
                1
            ),
            Err(CursorError::CursorInvalid)
        );
        append(&mut buffer, "change", json!({}));
    }

    #[test]
    fn overflow_preserves_ready_gap_and_ended_markers() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        append(&mut buffer, "subscription.ready", json!({}));
        append(&mut buffer, "subscription.ready", json!({}));
        for index in 0..MAX_BUFFER_EVENTS + 1 {
            append(&mut buffer, "change", json!({"index": index}));
        }
        append(
            &mut buffer,
            "subscription.ended",
            json!({"reason": "closed"}),
        );
        append(
            &mut buffer,
            "subscription.ended",
            json!({"reason": "closed"}),
        );

        let names = read_names(&buffer, &start);
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "subscription.ready")
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "subscription.gap")
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "subscription.ended")
                .count(),
            1
        );
        assert_eq!(buffer.event_count(), MAX_BUFFER_EVENTS);
    }

    #[test]
    fn repeated_lag_merges_gaps_and_keeps_both_bounds() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        for index in 0..MAX_BUFFER_EVENTS * 3 {
            append(&mut buffer, "change", json!({"index": index}));
        }

        let events = buffer.read(Some(&start), 1).unwrap();
        assert_eq!(events[0].meta.name, "subscription.gap");
        assert_eq!(
            events[0].data,
            json!({"missed": (MAX_BUFFER_EVENTS * 2) as u64})
        );
        assert_eq!(
            buffer
                .read(Some(&start), usize::MAX)
                .unwrap()
                .iter()
                .filter(|event| event.meta.name == "subscription.gap")
                .count(),
            1
        );
        assert_eq!(buffer.event_count(), MAX_BUFFER_EVENTS);
        assert!(buffer.bytes() <= MAX_BUFFER_BYTES);
    }

    #[test]
    fn appended_lag_gaps_merge_with_an_exact_total() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        append(&mut buffer, "change", json!({}));
        append(&mut buffer, "subscription.gap", json!({"missed": 3}));
        let last = append(&mut buffer, "subscription.gap", json!({"missed": 4}));

        let events = buffer.read(Some(&start), 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].data, json!({"missed": 7}));
        assert_eq!(events[1].meta.cursor, last);
        assert!(buffer.read(Some(&last), 10).unwrap().is_empty());
    }

    #[test]
    fn byte_overflow_evicts_the_oldest_event() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        let payload = "x".repeat(MAX_BUFFER_BYTES / 3);
        for name in ["first", "second", "third"] {
            append(&mut buffer, name, json!({"payload": payload.as_str()}));
        }

        let events = buffer.read(Some(&start), 10).unwrap();
        assert_eq!(events[0].meta.name, "subscription.gap");
        assert_eq!(events[0].data, json!({"missed": 1}));
        assert_eq!(events[1].meta.name, "second");
        assert_eq!(events[2].meta.name, "third");
        assert!(buffer.bytes() <= MAX_BUFFER_BYTES);
    }

    #[test]
    fn ack_never_moves_the_consumed_watermark_backwards_or_past_head() {
        let mut buffer = EventBuffer::new();
        append(&mut buffer, "first", json!({}));
        append(&mut buffer, "second", json!({}));
        buffer.ack(2);
        buffer.ack(1);
        buffer.ack(2);

        assert_eq!(buffer.consumed(), 2);
        buffer.ack(u64::MAX);
        assert_eq!(buffer.consumed(), buffer.head());
    }

    #[test]
    fn replay_and_limited_reads_do_not_advance_the_consumed_watermark() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        append(&mut buffer, "first", json!({}));
        append(&mut buffer, "second", json!({}));
        append(&mut buffer, "third", json!({}));

        let first_read = buffer.read(Some(&start), 2).unwrap();
        let replay = buffer.read(Some(&start), 1).unwrap();
        assert_eq!(buffer.consumed(), 0);
        assert_eq!(cursor_seq(&replay[0]), 1);
        buffer.ack(cursor_seq(first_read.last().unwrap()));
        assert_eq!(buffer.consumed(), 2);
    }

    #[test]
    fn concurrent_appends_and_reads_keep_sequences_increasing() {
        let buffer = Arc::new(Mutex::new(EventBuffer::new()));
        let running = Arc::new(AtomicBool::new(true));
        let writer_buffer = Arc::clone(&buffer);
        let writer_running = Arc::clone(&running);
        let writer = thread::spawn(move || {
            for index in 0..100 {
                append(
                    &mut writer_buffer.lock().unwrap(),
                    "change",
                    json!({"index": index}),
                );
                thread::yield_now();
            }
            writer_running.store(false, Ordering::Release);
        });

        let start = buffer.lock().unwrap().start_cursor();
        let mut cursor = start;
        let mut previous = 0;
        while running.load(Ordering::Acquire) {
            let events = buffer.lock().unwrap().read(Some(&cursor), 20).unwrap();
            for event in events {
                let seq = cursor_seq(&event);
                assert!(seq > previous);
                previous = seq;
                cursor = event.meta.cursor;
            }
            thread::yield_now();
        }
        writer.join().unwrap();
        let events = buffer.lock().unwrap().read(Some(&cursor), 200).unwrap();
        for event in events {
            let seq = cursor_seq(&event);
            assert!(seq > previous);
            previous = seq;
        }
        assert_eq!(previous, 100);
    }
}
