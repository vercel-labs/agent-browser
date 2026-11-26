use std::collections::VecDeque;

use serde_json::json;

use super::envelope::EventEnvelope;

pub const MAX_BUFFER_EVENTS: usize = 200;
pub const MAX_BUFFER_BYTES: usize = 1_048_576;
const MAX_GAP_RUNS: usize = MAX_BUFFER_EVENTS;

struct Entry {
    start_seq: u64,
    end_seq: u64,
    event: EventEnvelope,
    bytes: usize,
    gap_runs: GapRuns,
}

/// One or more consecutive gap events, exact while they share a loss and a
/// lower bound once folding loses their individual losses.
#[derive(Clone, Debug)]
enum GapRun {
    Uniform {
        count: u64,
        per_seq: u64,
    },
    Folded {
        count: u64,
        total_missed: u64,
        min_per_seq: u64,
    },
}

impl GapRun {
    fn new(missed: u64) -> Self {
        Self::Uniform {
            count: 1,
            per_seq: missed,
        }
    }

    fn count(&self) -> u64 {
        match self {
            Self::Uniform { count, .. } => *count,
            Self::Folded { count, .. } => *count,
        }
    }

    fn total_missed(&self) -> u64 {
        match self {
            Self::Uniform { count, per_seq } => per_seq.saturating_mul(*count),
            Self::Folded { total_missed, .. } => *total_missed,
        }
    }

    fn min_per_seq(&self) -> u64 {
        match self {
            Self::Uniform { per_seq, .. } => *per_seq,
            Self::Folded { min_per_seq, .. } => *min_per_seq,
        }
    }

    fn append(&mut self, missed: u64) -> bool {
        match self {
            Self::Uniform { count, per_seq } if *per_seq == missed => {
                *count += 1;
                true
            }
            _ => false,
        }
    }

    /// Joins an adjacent run that reports the same per-event loss.
    fn absorb(&mut self, other: &GapRun) -> bool {
        match (self, other) {
            (
                Self::Uniform { count, per_seq },
                Self::Uniform {
                    count: other_count,
                    per_seq: other_per_seq,
                },
            ) if *per_seq == *other_per_seq => {
                *count += *other_count;
                true
            }
            _ => false,
        }
    }

    fn merge_folded(first: Self, second: Self) -> Self {
        Self::Folded {
            count: first.count() + second.count(),
            total_missed: first.total_missed() + second.total_missed(),
            min_per_seq: first.min_per_seq().min(second.min_per_seq()),
        }
    }

    /// Losses that a cursor `offset` events into this run has already been
    /// told about, never more than the run actually lost.
    fn already_missed_at(&self, offset: u64) -> u64 {
        let seen = offset + 1;
        match self {
            Self::Uniform { per_seq, .. } => per_seq.saturating_mul(seen),
            Self::Folded {
                count,
                total_missed,
                min_per_seq,
            } => {
                if seen >= *count {
                    *total_missed
                } else {
                    min_per_seq.saturating_mul(seen)
                }
            }
        }
    }
}

#[derive(Default)]
struct GapRuns {
    runs: VecDeque<GapRun>,
}

impl GapRuns {
    fn new(missed: u64) -> Self {
        let mut runs = VecDeque::new();
        runs.push_back(GapRun::new(missed));
        Self { runs }
    }

    fn push(&mut self, missed: u64) {
        if let Some(last) = self.runs.back_mut() {
            if last.append(missed) {
                return;
            }
        }
        self.runs.push_back(GapRun::new(missed));
        self.fold();
    }

    fn extend(&mut self, other: GapRuns) {
        for run in other.runs {
            self.push_run(run);
        }
        self.fold();
    }

    fn push_run(&mut self, run: GapRun) {
        if let Some(last) = self.runs.back_mut() {
            if last.absorb(&run) {
                return;
            }
        }
        self.runs.push_back(run);
    }

    fn len(&self) -> usize {
        self.runs.len()
    }

    fn total_missed(&self) -> u64 {
        self.runs.iter().map(GapRun::total_missed).sum()
    }

    fn missed_before(&self, index: usize) -> u64 {
        let index = index as u64;
        let mut seen_events = 0u64;
        let mut seen_missed = 0u64;
        for run in &self.runs {
            let end = seen_events + run.count();
            if index < end {
                return seen_missed + run.already_missed_at(index - seen_events);
            }
            seen_events = end;
            seen_missed += run.total_missed();
        }
        seen_missed
    }

    fn fold(&mut self) {
        while self.runs.len() > MAX_GAP_RUNS {
            let Some(first) = self.runs.pop_front() else {
                break;
            };
            let Some(second) = self.runs.pop_front() else {
                self.runs.push_front(first);
                break;
            };
            self.runs.push_front(GapRun::merge_folded(first, second));
        }
    }
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

    pub fn id(&self) -> &str {
        &self.subscription_id
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
                last.gap_runs.push(gap_missed(&event));
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
        let gap_runs = match name {
            "subscription.gap" => GapRuns::new(gap_missed(&event)),
            _ => GapRuns::default(),
        };
        self.event_count += usize::from(bytes > 0);
        self.event_bytes += bytes;
        entries.push_back(Entry {
            start_seq: seq,
            end_seq: seq,
            event,
            bytes,
            gap_runs,
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
                let missed = entry.gap_runs.total_missed();
                let already_missed = if since < entry.start_seq {
                    0
                } else {
                    entry
                        .gap_runs
                        .missed_before((since - entry.start_seq) as usize)
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
            .ok_or(CursorError)
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
            let mut missed = 1;
            let mut gap_runs = GapRuns::new(1);
            if index > 0 && entries[index - 1].event.meta.name == "subscription.gap" {
                let previous = entries.remove(index - 1).unwrap();
                index -= 1;
                removed.start_seq = previous.start_seq;
                missed += previous.gap_runs.total_missed();
                gap_runs = previous.gap_runs;
                removed.event = previous.event;
                gap_runs.push(1);
            }
            if index < entries.len() && entries[index].event.meta.name == "subscription.gap" {
                let next = entries.remove(index).unwrap();
                missed += next.gap_runs.total_missed();
                gap_runs.extend(next.gap_runs);
                removed.end_seq = next.end_seq;
            }
            let meta = &removed.event.meta;
            removed.event = EventEnvelope::new(
                "subscription.gap",
                &meta.session,
                &meta.tab,
                json!({"missed": missed}),
            );
            removed.gap_runs = gap_runs;
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
pub struct CursorError;

#[cfg(test)]
mod tests {
    use super::super::envelope::EventTab;
    use super::*;
    use serde_json::Value;

    fn append(buffer: &mut EventBuffer, name: &str, data: Value) -> String {
        let tab = EventTab {
            id: "tab".into(),
            label: "Tab".into(),
            target_id: "target".into(),
        };
        buffer.append(EventEnvelope::new(name, "session", &tab, data))
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
        let first = append(&mut buffer, "change", json!({}));
        append(&mut buffer, "subscription.gap", json!({"missed": 3}));
        append(&mut buffer, "subscription.gap", json!({"missed": 4}));
        let last = append(&mut buffer, "subscription.gap", json!({"missed": 5}));
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
        assert_eq!(buffer.read(None, 0).unwrap(), vec![]);
        let events = buffer.read(Some(&buffer.start_cursor()), 2).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| &event.meta.cursor)
                .collect::<Vec<_>>(),
            [&first, &last]
        );
        assert_eq!(events[1].data, json!({"missed": 12}));
        assert_eq!(buffer.read(None, 1).unwrap(), events[..1]);
        assert!(buffer.read(Some(&last), 1).unwrap().is_empty());
        assert_eq!(buffer.head_cursor(), last);
        assert_eq!(buffer.consumed(), 0);
        for seq in [2, 1, 2, u64::MAX] {
            buffer.ack(seq);
            assert_eq!(buffer.consumed(), seq.clamp(2, 4));
        }
        buffer.ack_cursor(&first).unwrap();
        assert_eq!(buffer.consumed(), buffer.head());
        for cursor in [
            "bad".to_string(),
            "foreign:1".to_string(),
            buffer.cursor(5),
            buffer.cursor(1).replace(":1", ":bad"),
        ] {
            assert_eq!(buffer.read(Some(&cursor), 1), Err(CursorError));
            assert_eq!(buffer.ack_cursor(&cursor), Err(CursorError));
        }
    }

    #[test]
    fn eviction_keeps_one_homogeneous_gap_run_after_5000_drops() {
        let mut buffer = EventBuffer::new();
        let start_cursor = buffer.start_cursor();
        let ready = append(&mut buffer, "subscription.ready", json!({}));
        assert_eq!(buffer.id(), start_cursor.split(':').next().unwrap());
        let evictions = 5000usize;
        for _ in 0..(MAX_BUFFER_EVENTS + evictions) {
            append(&mut buffer, "subscription.rearmed", json!({}));
        }
        {
            let entry = buffer
                .entries
                .iter()
                .find(|entry| entry.event.meta.name == "subscription.gap")
                .unwrap();
            assert_eq!(entry.gap_runs.len(), 1);
            let run = entry.gap_runs.runs.front().unwrap();
            assert!(matches!(run, GapRun::Uniform { .. }));
            assert_eq!(run.count(), evictions as u64);
            assert_eq!(run.total_missed(), evictions as u64);
            assert_eq!(run.min_per_seq(), 1);
            assert_eq!(entry.event.data, json!({"missed": evictions}));
        }
        assert_eq!(buffer.dropped(), evictions as u64);
        let events = buffer.read(Some(&start_cursor), 2).unwrap();
        assert_eq!(events[0].meta.cursor, ready);
        assert_eq!(events[1].meta.name, "subscription.gap");
        assert_eq!(events[1].data, json!({"missed": evictions}));
        assert_eq!(
            buffer.read(Some(&buffer.cursor(5000)), 1).unwrap()[0].data,
            json!({"missed": 1})
        );
    }

    /// A cursor inside a folded run of mixed sizes may have folded losses
    /// reported to it twice, but never underreports the ones it has not seen.
    #[test]
    fn folded_heterogeneous_gap_never_underreports_unread_losses() {
        let mut buffer = EventBuffer::new();
        append(&mut buffer, "change", json!({}));
        let mut missed_by_seq = vec![0u64];
        for index in 0..MAX_GAP_RUNS as u64 + 10 {
            let missed = if index % 2 == 0 { 10 } else { 2 };
            append(&mut buffer, "subscription.gap", json!({ "missed": missed }));
            missed_by_seq.push(missed);
        }
        let head = buffer.head();
        assert_eq!(head, missed_by_seq.len() as u64);
        let total: u64 = missed_by_seq.iter().sum();

        let (folded_count, folded_total, folded_min) = {
            let entry = buffer.entries.back().unwrap();
            assert_eq!(entry.start_seq, 2);
            assert_eq!(entry.end_seq, head);
            assert_eq!(entry.gap_runs.len(), MAX_GAP_RUNS);
            match entry.gap_runs.runs.front().unwrap() {
                GapRun::Folded {
                    count,
                    total_missed,
                    min_per_seq,
                } => (*count, *total_missed, *min_per_seq),
                other => panic!("the gap prefix should be folded, got {other:?}"),
            }
        };
        let folded_end = 1 + folded_count;
        assert!(folded_count > 1, "one run has nothing to fold");
        assert!(folded_end < head, "the tail should stay unfolded");
        assert_eq!(
            folded_total,
            missed_by_seq[1..=folded_count as usize].iter().sum::<u64>()
        );
        assert_eq!(folded_min, 2);

        let unread = |seq: u64| missed_by_seq[seq as usize..].iter().sum::<u64>();
        let reported = |seq: u64| {
            buffer.read(Some(&buffer.cursor(seq)), 1).unwrap()[0].data["missed"]
                .as_u64()
                .unwrap()
        };

        assert_eq!(reported(1), total);
        for seq in 2..folded_end {
            let (reported, unread) = (reported(seq), unread(seq));
            assert!(unread > 0);
            assert!(
                reported >= unread,
                "cursor {seq} reported {reported} of {unread} unread losses"
            );
            assert!(
                reported > 0,
                "cursor {seq} reported no losses while {unread} were unread"
            );
        }
        for seq in folded_end..head {
            assert_eq!(reported(seq), unread(seq), "cursor {seq}");
        }
        assert!(buffer
            .read(Some(&buffer.head_cursor()), 1)
            .unwrap()
            .is_empty());
    }
}
