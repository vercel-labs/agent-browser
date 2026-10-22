use std::collections::VecDeque;

use serde_json::json;

use super::envelope::EventEnvelope;

pub const MAX_BUFFER_EVENTS: usize = 200;
pub const MAX_BUFFER_BYTES: usize = 1_048_576;

struct Entry {
    start_seq: u64,
    end_seq: u64,
    event: EventEnvelope,
    bytes: usize,
    gap_prefix: Vec<u64>,
}

pub struct EventBuffer {
    subscription_id: String,
    entries: VecDeque<Entry>,
    consumed: u64,
    event_count: usize,
    event_bytes: usize,
    dropped: u64,
}

impl EventBuffer {
    pub fn new() -> Self {
        Self {
            subscription_id: uuid::Uuid::new_v4().simple().to_string(),
            entries: VecDeque::new(),
            consumed: 0,
            event_count: 0,
            event_bytes: 0,
            dropped: 0,
        }
    }

    pub fn append(&mut self, mut event: EventEnvelope) -> String {
        let seq = self.head() + 1;
        let cursor = self.cursor(seq);
        let entries = &mut self.entries;
        let name = event.meta.name.as_str();
        let existing = match name {
            "subscription.ready" | "subscription.ended" => entries
                .iter_mut()
                .find(|entry| entry.event.meta.name == name),
            "subscription.gap" => entries
                .back_mut()
                .filter(|last| last.event.meta.name == name),
            _ => None,
        };
        if let Some(last) = existing {
            if name == "subscription.gap" {
                let missed = gap_missed(&last.event) + gap_missed(&event);
                last.event.data = json!({"missed": missed});
                last.end_seq = seq;
                last.gap_prefix.push(missed);
                last.event.meta.cursor = cursor;
            }
            return last.event.meta.cursor.clone();
        }
        event.meta.cursor = cursor.clone();
        event.meta.subscription_id = self.subscription_id.clone();
        let bytes = match name {
            "subscription.ready" | "subscription.gap" | "subscription.ended" => 0,
            _ => serde_json::to_vec(&event)
                .expect("event envelopes always serialize")
                .len(),
        };
        let gap_prefix = match name {
            "subscription.gap" => vec![gap_missed(&event)],
            _ => Vec::new(),
        };
        self.event_count += usize::from(bytes > 0);
        self.event_bytes += bytes;
        entries.push_back(Entry {
            start_seq: seq,
            end_seq: seq,
            event,
            bytes,
            gap_prefix,
        });
        self.enforce_bounds();
        cursor
    }

    pub fn read(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, CursorError> {
        let since = since.map_or(Ok(0), |cursor| self.parse_cursor(cursor))?;
        let mut events = Vec::new();
        let unread = self.entries.iter().filter(|entry| entry.end_seq > since);
        for entry in unread.take(limit) {
            let mut event = entry.event.clone();
            if entry.event.meta.name == "subscription.gap" {
                let missed = gap_missed(&entry.event);
                let already_missed = if since < entry.start_seq {
                    0
                } else {
                    entry
                        .gap_prefix
                        .get((since - entry.start_seq) as usize)
                        .copied()
                        .unwrap_or(missed)
                };
                event.data = json!({"missed": missed.saturating_sub(already_missed)});
            }
            events.push(event);
        }
        Ok(events)
    }

    pub fn ack(&mut self, seq: u64) {
        self.consumed = self.consumed.max(seq.min(self.head()));
    }

    pub fn ack_cursor(&mut self, cursor: &str) -> Result<(), CursorError> {
        self.ack(self.parse_cursor(cursor)?);
        Ok(())
    }

    pub fn start_cursor(&self) -> String {
        self.cursor(0)
    }

    pub fn head(&self) -> u64 {
        self.entries.back().map_or(0, |last| last.end_seq)
    }

    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn head_cursor(&self) -> String {
        self.cursor(self.head())
    }

    pub fn consumed_cursor(&self) -> String {
        self.cursor(self.consumed)
    }

    fn cursor(&self, seq: u64) -> String {
        format!("{}:{seq}", self.subscription_id)
    }

    fn parse_cursor(&self, cursor: &str) -> Result<u64, CursorError> {
        cursor
            .rsplit_once(':')
            .filter(|(id, _)| *id == self.subscription_id)
            .and_then(|(_, seq)| seq.parse().ok())
            .filter(|seq| *seq <= self.head())
            .ok_or(CursorError::CursorInvalid)
    }

    fn enforce_bounds(&mut self) {
        let entries = &mut self.entries;
        while self.event_count > MAX_BUFFER_EVENTS || self.event_bytes > MAX_BUFFER_BYTES {
            let Some(mut index) = entries.iter().position(|entry| entry.bytes > 0) else {
                break;
            };
            let mut removed = entries.remove(index).unwrap();
            self.event_count -= 1;
            self.event_bytes -= removed.bytes;
            self.dropped += removed.end_seq - removed.start_seq + 1;
            let mut missed = 0;
            let mut gap_prefix = Vec::new();
            if index > 0 && entries[index - 1].event.meta.name == "subscription.gap" {
                let previous = entries.remove(index - 1).unwrap();
                index -= 1;
                removed.start_seq = previous.start_seq;
                missed = gap_missed(&previous.event);
                gap_prefix = previous.gap_prefix;
                removed.event = previous.event;
            }
            missed += 1;
            gap_prefix.push(missed);
            if index < entries.len() && entries[index].event.meta.name == "subscription.gap" {
                let next = entries.remove(index).unwrap();
                let base = missed;
                missed += gap_missed(&next.event);
                gap_prefix.extend(next.gap_prefix.into_iter().map(|count| base + count));
                removed.end_seq = next.end_seq;
            }
            let meta = &removed.event.meta;
            removed.event = EventEnvelope::new(
                "subscription.gap",
                &meta.session,
                &meta.tab.id,
                &meta.tab.label,
                &meta.tab.target_id,
                json!({"missed": missed}),
            );
            removed.gap_prefix = gap_prefix;
            removed.event.meta.subscription_id = self.subscription_id.clone();
            removed.event.meta.cursor = format!("{}:{}", self.subscription_id, removed.end_seq);
            removed.bytes = 0;
            entries.insert(index, removed);
        }
    }
}

impl Default for EventBuffer {
    fn default() -> Self {
        Self::new()
    }
}

fn gap_missed(event: &EventEnvelope) -> u64 {
    event.data["missed"].as_u64().unwrap_or(1)
}

#[derive(Debug, PartialEq, Eq)]
pub enum CursorError {
    CursorInvalid,
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cursor_invalid")
    }
}

impl std::error::Error for CursorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn append(buffer: &mut EventBuffer, name: &str, data: Value) -> String {
        buffer.append(EventEnvelope::new(
            name, "session", "tab", "Tab", "target", data,
        ))
    }

    #[test]
    fn bounds_preserve_markers_and_merge_gaps() {
        for (count, data, dropped) in [
            (
                MAX_BUFFER_EVENTS * 3,
                json!({}),
                (MAX_BUFFER_EVENTS * 2) as u64,
            ),
            (4, json!("x".repeat(MAX_BUFFER_BYTES / 3)), 2),
        ] {
            let mut buffer = EventBuffer::new();
            let ready = append(&mut buffer, "subscription.ready", json!({}));
            let mut previous = ready.clone();
            for index in 0..count {
                let cursor = append(&mut buffer, "subscription.rearmed", data.clone());
                assert_eq!(cursor, buffer.cursor((index + 2) as u64));
                let next = buffer.read(Some(&previous), 1).unwrap();
                assert_eq!(next[0].meta.cursor, cursor);
                previous = cursor;
            }
            let ended = append(
                &mut buffer,
                "subscription.ended",
                json!({"reason": "closed"}),
            );
            let events = buffer.read(None, usize::MAX).unwrap();
            assert_eq!(events.len(), count - dropped as usize + 3);
            assert_eq!(events[0].meta.cursor, ready);
            assert_eq!(events[0].meta.subscription_id, buffer.subscription_id);
            assert_eq!(events[1].meta.name, "subscription.gap");
            assert_eq!(events[1].data, json!({"missed": dropped}));
            assert_eq!(events.last().unwrap().meta.cursor, ended);
            assert_eq!(buffer.dropped(), dropped);
            assert!(
                buffer.event_count <= MAX_BUFFER_EVENTS && buffer.event_bytes <= MAX_BUFFER_BYTES
            );
            assert_eq!(
                append(&mut buffer, "subscription.ready", Value::Null),
                ready
            );
            assert_eq!(
                append(&mut buffer, "subscription.ended", Value::Null),
                ended
            );
            assert_eq!(buffer.head(), (count + 2) as u64);
            assert_eq!(
                buffer.read(Some(&buffer.cursor(2)), 1).unwrap()[0].data["missed"],
                dropped - 1
            );
            let resumed = buffer.read(Some(&events[1].meta.cursor), 1).unwrap();
            assert!(resumed[0].data == data);
        }
    }

    #[test]
    fn merged_gaps_report_only_the_unread_suffix() {
        let mut buffer = EventBuffer::new();
        append(&mut buffer, "change", json!({}));
        append(&mut buffer, "subscription.gap", json!({"missed": 3}));
        append(&mut buffer, "subscription.gap", json!({"missed": 4}));
        append(&mut buffer, "subscription.gap", json!({"missed": 5}));
        assert_eq!(
            buffer.read(Some(&buffer.cursor(2)), 1).unwrap()[0].data,
            json!({"missed": 9})
        );
        assert_eq!(
            buffer.read(Some(&buffer.cursor(3)), 1).unwrap()[0].data,
            json!({"missed": 5})
        );
        assert_eq!(
            buffer.read(Some(&buffer.cursor(1)), 1).unwrap()[0].data,
            json!({"missed": 12})
        );
    }

    #[test]
    fn gap_totals_replay_and_acknowledgement() {
        let mut buffer = EventBuffer::new();
        let first = append(&mut buffer, "change", json!({}));
        append(&mut buffer, "subscription.gap", json!({"missed": 3}));
        let last = append(&mut buffer, "subscription.gap", json!({"missed": 4}));
        assert_eq!(buffer.read(None, 0).unwrap(), vec![]);
        let events = buffer.read(Some(&buffer.start_cursor()), 2).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| &event.meta.cursor)
                .collect::<Vec<_>>(),
            [&first, &last]
        );
        assert_eq!(events[1].data, json!({"missed": 7}));
        assert_eq!(
            buffer.read(Some(&buffer.cursor(2)), 1).unwrap()[0].data,
            json!({"missed": 4})
        );
        assert_eq!(buffer.read(None, 1).unwrap(), events[..1]);
        assert!(buffer.read(Some(&last), 1).unwrap().is_empty());
        assert_eq!(buffer.head_cursor(), last);
        assert_eq!(buffer.consumed(), 0);
        for seq in [2, 1, 2, u64::MAX] {
            buffer.ack(seq);
            assert_eq!(buffer.consumed(), seq.clamp(2, 3));
        }
        buffer.ack_cursor(&first).unwrap();
        assert_eq!(buffer.consumed(), buffer.head());
        for cursor in [
            "bad".to_string(),
            "foreign:1".to_string(),
            buffer.cursor(4),
            buffer.cursor(1).replace(":1", ":bad"),
        ] {
            assert_eq!(
                buffer.read(Some(&cursor), 1),
                Err(CursorError::CursorInvalid)
            );
            assert_eq!(buffer.ack_cursor(&cursor), Err(CursorError::CursorInvalid));
        }
        assert_eq!(CursorError::CursorInvalid.to_string(), "cursor_invalid");
    }
}
