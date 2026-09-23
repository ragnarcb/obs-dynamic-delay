//! The delay engine: buffers incoming media and releases it after the current
//! delay, changing the delay on the fly without restarting the stream.
//!
//! * Increasing the delay ("grow"): output continues until the next keyframe,
//!   then that keyframe is shown frozen (plus silent audio) until the buffer
//!   holds the new delay. Viewers see a short freeze, the stream never drops.
//! * Decreasing the delay ("shrink"): the buffer is cut at the newest keyframe
//!   that is already due under the new delay. Viewers see a jump cut forward.
//!
//! Output timestamps are rewritten so they stay continuous and monotonic.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::Serialize;

use crate::flv::{self, AacInfo, Kind};

/// Gap (ms) inserted in the output timeline after a cut.
const CUT_GAP_MS: u64 = 50;

#[derive(Debug, Clone)]
pub struct OutPacket {
    pub kind: Kind,
    pub ts: u32,
    pub data: Bytes,
}

struct Queued {
    kind: Kind,
    ts: u64,
    data: Bytes,
    arrival: Instant,
    key: bool,
    header: bool,
}

struct Fill {
    key: Option<Bytes>,
    started: Instant,
    base: u64,
    video_n: u64,
    audio_n: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Live,
    Delayed,
    /// Waiting for a keyframe to start building up more delay.
    Growing,
    /// Showing a frozen frame while the buffer fills up.
    Filling,
    /// Waiting for a keyframe to cut the delay down.
    Shrinking,
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineStatus {
    pub phase: Phase,
    pub target_ms: u64,
    pub current_ms: u64,
    pub buffered_ms: u64,
    pub buffered_bytes: usize,
}

pub struct Engine {
    queue: VecDeque<Queued>,
    queued_bytes: usize,
    target: Duration,
    eff: Duration,
    fill: Option<Fill>,
    offset: i64,
    rebase_next: bool,
    last_out: [Option<u64>; 2],
    last_any: u64,
    max_video_pts: Option<u64>,
    header_seen: [bool; 2],
    bypass: Vec<OutPacket>,
    aac: Option<AacInfo>,
    has_video: bool,
    filler_interval_ms: u64,
    last_sent_arrival: Option<Instant>,
}

impl Engine {
    pub fn new(filler_fps: u32) -> Self {
        Engine {
            queue: VecDeque::new(),
            queued_bytes: 0,
            target: Duration::ZERO,
            eff: Duration::ZERO,
            fill: None,
            offset: 0,
            rebase_next: false,
            last_out: [None, None],
            last_any: 0,
            max_video_pts: None,
            header_seen: [false, false],
            bypass: Vec::new(),
            aac: None,
            has_video: false,
            filler_interval_ms: 1000 / filler_fps.clamp(1, 60) as u64,
            last_sent_arrival: None,
        }
    }

    pub fn set_target(&mut self, target: Duration) {
        self.target = target;
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.fill.is_none() && self.bypass.is_empty()
    }

    pub fn push(&mut self, kind: Kind, ts: u64, data: Bytes, arrival: Instant) {
        let (header, key) = match kind {
            Kind::Video => {
                self.has_video = true;
                (flv::video_is_sequence_header(&data), flv::video_is_keyframe(&data))
            }
            Kind::Audio => (flv::audio_is_sequence_header(&data), false),
        };
        if header && kind == Kind::Audio {
            self.aac = flv::parse_aac_config(&data);
        }
        // The very first decoder config of each track is sent right away so the
        // upstream can always decode filler frames, even at stream start.
        if header && !self.header_seen[kind.idx()] {
            self.header_seen[kind.idx()] = true;
            self.bypass.push(OutPacket { kind, ts: self.last_any as u32, data });
            return;
        }
        self.queued_bytes += data.len();
        self.queue.push_back(Queued { kind, ts, data, arrival, key, header });
    }

    pub fn poll(&mut self, now: Instant, out: &mut Vec<OutPacket>) {
        out.append(&mut self.bypass);
        loop {
            if self.fill.is_some() {
                if !self.step_fill(now, out) {
                    return;
                }
                continue;
            }
            if self.target < self.eff {
                self.try_shrink(now);
            }
            while let Some(head) = self.queue.front() {
                if head.arrival + self.eff > now {
                    return;
                }
                let can_freeze = (head.kind == Kind::Video && head.key) || !self.has_video;
                if self.target > self.eff && can_freeze {
                    self.start_fill(now);
                    break;
                }
                let q = self.pop().unwrap();
                self.emit(q, out);
            }
            if self.fill.is_none() {
                return;
            }
        }
    }

    pub fn status(&self, now: Instant) -> EngineStatus {
        let phase = if self.fill.is_some() {
            Phase::Filling
        } else if self.target > self.eff {
            Phase::Growing
        } else if self.target < self.eff {
            Phase::Shrinking
        } else if self.eff.is_zero() {
            Phase::Live
        } else {
            Phase::Delayed
        };
        let current = if self.fill.is_some() {
            self.queue.front().map(|h| now.saturating_duration_since(h.arrival))
        } else {
            self.last_sent_arrival.map(|a| now.saturating_duration_since(a))
        };
        let buffered = match (self.queue.front(), self.queue.back()) {
            (Some(a), Some(b)) => b.arrival.saturating_duration_since(a.arrival),
            _ => Duration::ZERO,
        };
        EngineStatus {
            phase,
            target_ms: self.target.as_millis() as u64,
            current_ms: current.unwrap_or(self.eff).as_millis() as u64,
            buffered_ms: buffered.as_millis() as u64,
            buffered_bytes: self.queued_bytes,
        }
    }

    fn pop(&mut self) -> Option<Queued> {
        let q = self.queue.pop_front()?;
        self.queued_bytes -= q.data.len();
        Some(q)
    }

    fn emit(&mut self, q: Queued, out: &mut Vec<OutPacket>) {
        let cts = if q.kind == Kind::Video { flv::video_cts(&q.data) } else { 0 };
        let ts = if q.header {
            self.last_any
        } else {
            if self.rebase_next {
                self.offset = self.cut_base(cts) as i64 - q.ts as i64;
                self.rebase_next = false;
            }
            (q.ts as i64 + self.offset).max(0) as u64
        };
        self.last_sent_arrival = Some(q.arrival);
        if let Some(ts) = self.stamp(q.kind, ts, q.header, cts) {
            out.push(OutPacket { kind: q.kind, ts: ts as u32, data: q.data });
        }
    }

    /// Enforces a monotonic timeline per track. Returns None if the packet must be dropped.
    fn stamp(&mut self, kind: Kind, mut ts: u64, header: bool, cts: i64) -> Option<u64> {
        let i = kind.idx();
        if let Some(last) = self.last_out[i]
            && ts < last {
                if kind == Kind::Audio && !header {
                    return None;
                }
                ts = last;
            }
        self.last_out[i] = Some(ts);
        self.last_any = self.last_any.max(ts);
        if kind == Kind::Video && !header {
            let pts = (ts as i64 + cts).max(0) as u64;
            self.max_video_pts = Some(self.max_video_pts.map_or(pts, |m| m.max(pts)));
        }
        Some(ts)
    }

    /// First output DTS after a discontinuity, chosen so that the next keyframe
    /// (with composition offset `key_cts`) is displayed after everything already sent.
    fn cut_base(&self, key_cts: i64) -> u64 {
        let mut base = self.last_any + CUT_GAP_MS;
        if let Some(pts) = self.max_video_pts {
            base = base.max((pts as i64 + CUT_GAP_MS as i64 - key_cts).max(0) as u64);
        }
        base
    }

    fn start_fill(&mut self, now: Instant) {
        let head = self.queue.front().unwrap();
        let key = (head.kind == Kind::Video).then(|| head.data.clone());
        let key_cts = key.as_deref().map_or(0, flv::video_cts);
        let base = if self.last_out.iter().any(Option::is_some) {
            self.cut_base(key_cts)
        } else {
            0
        };
        self.fill = Some(Fill { key, started: now, base, video_n: 0, audio_n: 0 });
    }

    /// Emits filler frames. Returns true once the buffer is full and normal output resumes.
    fn step_fill(&mut self, now: Instant, out: &mut Vec<OutPacket>) -> bool {
        let mut f = self.fill.take().unwrap();
        let elapsed = now.saturating_duration_since(f.started).as_millis() as u64;

        if let Some(key) = &f.key {
            let cts = flv::video_cts(key);
            while f.video_n * self.filler_interval_ms <= elapsed {
                let ts = f.base + f.video_n * self.filler_interval_ms;
                f.video_n += 1;
                if let Some(ts) = self.stamp(Kind::Video, ts, false, cts) {
                    out.push(OutPacket { kind: Kind::Video, ts: ts as u32, data: key.clone() });
                }
            }
        }
        if let Some(aac) = self.aac {
            let silent = flv::silent_aac_frame(&aac);
            loop {
                let rel = f.audio_n * 1024 * 1000 / aac.sample_rate as u64;
                if rel > elapsed {
                    break;
                }
                f.audio_n += 1;
                if let Some(ts) = self.stamp(Kind::Audio, f.base + rel, false, 0) {
                    out.push(OutPacket { kind: Kind::Audio, ts: ts as u32, data: silent.clone() });
                }
            }
        }

        let Some(head) = self.queue.front() else {
            self.fill = Some(f);
            return false;
        };
        if head.arrival + self.target > now {
            self.fill = Some(f);
            return false;
        }
        // The buffer now holds the target delay: resume from the frozen keyframe.
        self.eff = now.saturating_duration_since(head.arrival).max(self.target);
        let head_cts = if head.kind == Kind::Video { flv::video_cts(&head.data) } else { 0 };
        let resume_ts = (f.base + elapsed).max(self.cut_base(head_cts));
        self.offset = resume_ts as i64 - head.ts as i64;
        self.rebase_next = false;
        true
    }

    fn try_shrink(&mut self, now: Instant) {
        if !self.has_video {
            let mut dropped = false;
            while let Some(h) = self.queue.front() {
                if h.arrival + self.target >= now || h.header {
                    break;
                }
                self.pop();
                dropped = true;
            }
            self.rebase_next |= dropped;
            self.eff = self.target;
            return;
        }

        let mut cut = None;
        for (i, q) in self.queue.iter().enumerate() {
            if q.arrival + self.target > now {
                break;
            }
            if q.kind == Kind::Video && q.key {
                cut = Some(i);
            }
        }
        let Some(cut) = cut else {
            if self.queue.is_empty() {
                self.eff = self.target;
            }
            return;
        };
        if cut > 0 {
            // Keep decoder config changes that happened inside the dropped range.
            let mut headers = Vec::new();
            for _ in 0..cut {
                let q = self.pop().unwrap();
                if q.header {
                    headers.push(q);
                }
            }
            for h in headers.into_iter().rev() {
                self.queued_bytes += h.data.len();
                self.queue.push_front(h);
            }
            self.rebase_next = true;
        }
        self.eff = self.target;
    }
}

/// Turns 32-bit RTMP timestamps from successive publish sessions into one
/// continuous 64-bit timeline.
pub struct TsUnwrapper {
    session: u64,
    last_raw: u32,
    cur: i64,
    max_seen: i64,
}

impl TsUnwrapper {
    pub fn new() -> Self {
        TsUnwrapper { session: u64::MAX, last_raw: 0, cur: 0, max_seen: -1 }
    }

    pub fn unwrap(&mut self, session: u64, raw: u32) -> u64 {
        if session != self.session {
            self.session = session;
            self.cur = if self.max_seen < 0 { 0 } else { self.max_seen + 40 };
        } else {
            self.cur += raw.wrapping_sub(self.last_raw) as i32 as i64;
        }
        self.last_raw = raw;
        self.cur = self.cur.max(0);
        self.max_seen = self.max_seen.max(self.cur);
        self.cur as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVC_HDR: [u8; 5] = [0x17, 0x00, 0, 0, 0];
    const AAC_HDR: [u8; 4] = [0xAF, 0x00, 0x11, 0x90];

    /// Simulates a 30 fps / 2 s GOP video with 48 kHz AAC audio.
    struct Sim {
        engine: Engine,
        t0: Instant,
        now_ms: u64,
        frame: u64,
        audio: u64,
        out: Vec<OutPacket>,
    }

    impl Sim {
        fn new() -> Self {
            let mut engine = Engine::new(2);
            let t0 = Instant::now();
            engine.push(Kind::Video, 0, Bytes::from_static(&AVC_HDR), t0);
            engine.push(Kind::Audio, 0, Bytes::from_static(&AAC_HDR), t0);
            Sim { engine, t0, now_ms: 0, frame: 0, audio: 0, out: Vec::new() }
        }

        fn run(&mut self, ms: u64) {
            let end = self.now_ms + ms;
            while self.now_ms < end {
                self.now_ms += 1;
                let now = self.t0 + Duration::from_millis(self.now_ms);
                while self.frame * 1000 / 30 <= self.now_ms {
                    let key = self.frame % 60 == 0;
                    let data = if key { vec![0x17, 1, 0, 0, 0] } else { vec![0x27, 1, 0, 0, 0] };
                    self.engine.push(Kind::Video, self.frame * 1000 / 30, data.into(), now);
                    self.frame += 1;
                }
                while self.audio * 1024 * 1000 / 48000 <= self.now_ms {
                    let ts = self.audio * 1024 * 1000 / 48000;
                    self.engine.push(Kind::Audio, ts, Bytes::from_static(&[0xAF, 1, 0x21]), now);
                    self.audio += 1;
                }
                self.engine.poll(now, &mut self.out);
            }
        }

        fn now(&self) -> Instant {
            self.t0 + Duration::from_millis(self.now_ms)
        }

        fn assert_monotonic(&self) {
            let mut last = [0u32; 2];
            for p in &self.out {
                assert!(p.ts >= last[p.kind.idx()], "non monotonic {:?} {} < {}", p.kind, p.ts, last[p.kind.idx()]);
                last[p.kind.idx()] = p.ts;
            }
        }
    }

    #[test]
    fn passthrough_when_live() {
        let mut s = Sim::new();
        s.run(3000);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Live);
        assert!(st.current_ms <= 40, "{st:?}");
        s.assert_monotonic();
    }

    #[test]
    fn grow_then_shrink() {
        let mut s = Sim::new();
        s.run(3000);
        s.engine.set_target(Duration::from_secs(10));
        s.run(1000);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Filling);
        s.run(11_000);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Delayed, "{st:?}");
        assert!((9_900..=10_100).contains(&st.current_ms), "{st:?}");
        // output timeline keeps running in real time during the freeze
        let last_video = s.out.iter().rev().find(|p| p.kind == Kind::Video).unwrap().ts as i64;
        assert!((last_video - s.now_ms as i64).abs() < 300, "video ts {last_video} vs clock {}", s.now_ms);

        s.engine.set_target(Duration::ZERO);
        s.run(2500);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Live, "{st:?}");
        assert!(st.current_ms < 2100, "{st:?}");
        assert!(st.buffered_ms < 2100, "{st:?}");
        s.assert_monotonic();
    }

    #[test]
    fn delay_from_stream_start() {
        let mut engine = Engine::new(2);
        engine.set_target(Duration::from_secs(5));
        let t0 = Instant::now();
        let mut out = Vec::new();
        engine.push(Kind::Video, 0, Bytes::from_static(&AVC_HDR), t0);
        engine.push(Kind::Video, 0, Bytes::from_static(&[0x17, 1, 0, 0, 0]), t0);
        engine.poll(t0, &mut out);
        // decoder config goes out immediately, then the first frame starts the freeze
        assert_eq!(out.len(), 2);
        assert_eq!(engine.status(t0).phase, Phase::Filling);
    }

    #[test]
    fn unwrapper_handles_wrap_and_new_sessions() {
        let mut u = TsUnwrapper::new();
        assert_eq!(u.unwrap(1, 0), 0);
        assert_eq!(u.unwrap(1, 1000), 1000);
        assert_eq!(u.unwrap(1, 990), 990);
        let (mut raw, mut prev) = (990u32, 990u64);
        for _ in 0..10 {
            raw = raw.wrapping_add(1_000_000_000);
            let v = u.unwrap(1, raw);
            assert_eq!(v - prev, 1_000_000_000);
            prev = v;
        }
        let next_session = u.unwrap(2, 0);
        assert!(next_session > prev);
    }
}
