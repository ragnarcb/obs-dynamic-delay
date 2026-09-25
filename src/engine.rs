//! The delay engine: buffers incoming media and releases it after the current
//! delay, changing the delay on the fly without restarting the stream.
//!
//! * Increasing the delay ("grow"), depending on [`GrowMode`]:
//!   - `Rewind`: jump back in the recently sent history and play it again.
//!     Instant, no freeze; viewers see the last seconds a second time.
//!   - `Freeze`: at the next keyframe, show that frame frozen (plus silent
//!     audio) until the buffer holds the new delay.
//!   - `Scene`: like `Freeze`, but the frozen frame is the first keyframe
//!     encoded after OBS switched to a chosen scene (orchestrated through
//!     [`SceneEvent`]s, handled by the OBS script).
//!
//! It also offers, on top of the delay buffer:
//! * [`Engine::censor`]: drop the newest, not yet aired seconds ("delete before it airs");
//! * [`Engine::replay`]: replay the last seconds on air, then return to the normal delay;
//! * [`Engine::snapshot`]: the most recent seconds of input, for saving a clip.
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

#[derive(Clone)]
struct Queued {
    kind: Kind,
    ts: u64,
    data: Bytes,
    arrival: Instant,
    key: bool,
    header: bool,
    /// First packet after a censored gap: rebase the timeline here.
    cut_before: bool,
}

struct Fill {
    key: Option<Bytes>,
    started: Instant,
    base: u64,
    video_n: u64,
    audio_n: u64,
}

/// How the engine builds up extra delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrowMode {
    Rewind,
    Freeze,
    Scene,
}

impl GrowMode {
    pub fn parse(s: &str) -> GrowMode {
        match s {
            "freeze" => GrowMode::Freeze,
            "scene" => GrowMode::Scene,
            _ => GrowMode::Rewind,
        }
    }
}

/// Requests for OBS while growing in [`GrowMode::Scene`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneEvent {
    /// Switch OBS to the delay scene, then call [`Engine::scene_shown`].
    Show,
    /// The frame to freeze on was captured (or the grow was cancelled): switch back.
    Back,
}

enum SceneState {
    Idle,
    Requested { since: Instant },
    Capturing { since: Instant, after: Instant },
    Captured(Bytes),
    /// OBS could not show the scene: fall back to a plain freeze.
    Failed,
}

/// Time between OBS reporting the scene switch and the first frame we accept
/// from it (covers the scene transition and the encoder pipeline).
const SCENE_SETTLE: Duration = Duration::from_millis(700);
/// Give up on the scene and freeze normally after this long.
const SCENE_TIMEOUT: Duration = Duration::from_secs(6);

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
    pub replaying: bool,
}

/// Recent input, used to write a clip file.
pub struct Clip {
    pub video_header: Option<Bytes>,
    pub audio_header: Option<Bytes>,
    /// (kind, input timestamp in ms, FLV tag body), starting at a keyframe.
    pub packets: Vec<(Kind, u64, Bytes)>,
}

pub struct Engine {
    queue: VecDeque<Queued>,
    queued_bytes: usize,
    /// Delay set by the streamer; `target` adds a running replay on top.
    base_target: Duration,
    replay_extra: Duration,
    replay_until: Option<Instant>,
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
    grow_mode: GrowMode,
    /// Recently sent packets, kept for [`GrowMode::Rewind`].
    history: VecDeque<Queued>,
    history_window: Duration,
    /// Arrival of the first packet after the last cut (delay shortened, censor):
    /// a rewind must not replay across a cut, the dropped part would leave a hole
    /// in the timeline and players stall waiting for it.
    rewind_floor: Option<Instant>,
    rewound_for: Option<Duration>,
    scene: SceneState,
    scene_events: Vec<SceneEvent>,
    /// After a censor: drop input until the next keyframe.
    await_key: bool,
    /// After a censor: fill the gap with the last sent keyframe.
    gap_fill_pending: bool,
    last_key_sent: Option<Bytes>,
    video_header: Option<Bytes>,
    audio_header: Option<Bytes>,
    /// OBS stopped publishing: a pending fill must not wait for more input.
    input_ended: bool,
}

impl Engine {
    pub fn new(filler_fps: u32) -> Self {
        Engine {
            queue: VecDeque::new(),
            queued_bytes: 0,
            base_target: Duration::ZERO,
            replay_extra: Duration::ZERO,
            replay_until: None,
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
            grow_mode: GrowMode::Freeze,
            history: VecDeque::new(),
            history_window: Duration::ZERO,
            rewind_floor: None,
            rewound_for: None,
            scene: SceneState::Idle,
            scene_events: Vec::new(),
            await_key: false,
            gap_fill_pending: false,
            last_key_sent: None,
            video_header: None,
            audio_header: None,
            input_ended: false,
        }
    }

    /// Tells the engine whether OBS is still publishing.
    pub fn set_input_ended(&mut self, ended: bool) {
        self.input_ended = ended;
    }

    /// Sets how extra delay is built. `history` is how much already sent media is
    /// kept (for rewinds, replays and clips).
    pub fn set_grow_mode(&mut self, mode: GrowMode, history: Duration) {
        self.grow_mode = mode;
        self.history_window = history;
    }

    /// Drops the newest `secs` of media that viewers have not seen yet. The aired
    /// stream holds its last frame (muted) over the gap and continues at the next
    /// keyframe, so the delay stays the same. Returns how much was removed.
    pub fn censor(&mut self, now: Instant, secs: Duration) -> Duration {
        let Some(from) = now.checked_sub(secs) else { return Duration::ZERO };
        let Some(i) = self.queue.iter().position(|q| !q.header && q.arrival >= from) else {
            return Duration::ZERO;
        };
        let removed = now.saturating_duration_since(self.queue[i].arrival);
        let tail: Vec<Queued> = self.queue.drain(i..).collect();
        for q in tail {
            self.queued_bytes -= q.data.len();
            if q.header {
                self.queued_bytes += q.data.len();
                self.queue.push_back(q);
            }
        }
        self.await_key = true;
        self.gap_fill_pending = self.last_key_sent.is_some();
        log::info!("censored {:.1}s before airing", removed.as_secs_f64());
        removed
    }

    /// Replays the last `secs` on air (from the sent history), then cuts back to
    /// the normal delay. Returns the length actually replayed.
    pub fn replay(&mut self, now: Instant, secs: Duration) -> Duration {
        if self.replay_until.is_some() || self.fill.is_some() {
            return Duration::ZERO;
        }
        let before = self.eff;
        self.replay_extra = secs;
        self.target = self.base_target + secs;
        self.try_rewind(now);
        let gained = self.eff.saturating_sub(before);
        if gained < Duration::from_secs(1) {
            self.replay_extra = Duration::ZERO;
            self.target = self.base_target;
            return Duration::ZERO;
        }
        self.replay_extra = gained;
        self.target = self.eff;
        self.rewound_for = Some(self.target);
        self.replay_until = Some(now + gained);
        log::info!("instant replay of {:.1}s", gained.as_secs_f64());
        gained
    }

    /// The most recent `secs` of input, starting at a keyframe. With `aired_only`
    /// it ends where the viewers are (the part still in the delay is left out).
    pub fn snapshot(&self, now: Instant, secs: Duration, aired_only: bool) -> Option<Clip> {
        let all: Vec<&Queued> = if aired_only {
            self.history.iter().filter(|q| !q.header).collect()
        } else {
            self.history.iter().chain(self.queue.iter()).filter(|q| !q.header).collect()
        };
        let end = if aired_only { all.last().map_or(now, |q| q.arrival) } else { now };
        let from = end.checked_sub(secs);
        let mut start = None;
        for (i, q) in all.iter().enumerate() {
            let starts = if self.has_video { q.kind == Kind::Video && q.key } else { true };
            if starts && (start.is_none() || from.is_some_and(|f| q.arrival <= f)) {
                start = Some(i);
            }
        }
        let start = start?;
        Some(Clip {
            video_header: self.video_header.clone(),
            audio_header: self.audio_header.clone(),
            packets: all[start..].iter().map(|q| (q.kind, q.ts, q.data.clone())).collect(),
        })
    }

    /// The codec header of the stream (AVC sequence header for H.264).
    pub fn video_header(&self) -> Option<&Bytes> {
        self.video_header.as_ref()
    }

    /// The audio codec header (AAC AudioSpecificConfig).
    pub fn audio_header(&self) -> Option<&Bytes> {
        self.audio_header.as_ref()
    }

    pub fn take_scene_events(&mut self) -> Vec<SceneEvent> {
        std::mem::take(&mut self.scene_events)
    }

    /// OBS switched to the delay scene at `at`.
    pub fn scene_shown(&mut self, at: Instant) {
        if let SceneState::Requested { since } = self.scene {
            self.scene = SceneState::Capturing { since, after: at + SCENE_SETTLE };
        }
    }

    /// OBS could not switch scenes: freeze on the live picture instead.
    pub fn scene_unavailable(&mut self) {
        if matches!(self.scene, SceneState::Requested { .. } | SceneState::Capturing { .. }) {
            log::warn!("delay scene unavailable, freezing the live picture instead");
            self.scene = SceneState::Failed;
        }
    }

    pub fn set_target(&mut self, target: Duration) {
        self.base_target = target;
        self.target = target + self.replay_extra;
    }

    /// A freeze, a scene hold or a replay is running.
    pub fn is_adjusting(&self) -> bool {
        self.fill.is_some() || self.replay_until.is_some()
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
        if header {
            match kind {
                Kind::Video => self.video_header = Some(data.clone()),
                Kind::Audio => self.audio_header = Some(data.clone()),
            }
        }
        let mut cut_before = false;
        if self.await_key && !header {
            if kind == Kind::Video && key {
                self.await_key = false;
                cut_before = true;
            } else {
                return; // still inside the censored part
            }
        }
        // The very first decoder config of each track is sent right away so the
        // upstream can always decode filler frames, even at stream start.
        if header && !self.header_seen[kind.idx()] {
            self.header_seen[kind.idx()] = true;
            self.bypass.push(OutPacket { kind, ts: self.last_any as u32, data });
            return;
        }
        if key
            && let SceneState::Capturing { after, .. } = self.scene
            && arrival >= after
        {
            self.scene = SceneState::Captured(data.clone());
            self.scene_events.push(SceneEvent::Back);
        }
        self.queued_bytes += data.len();
        self.queue.push_back(Queued { kind, ts, data, arrival, key, header, cut_before });
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
            self.trim_history(now);
            if self.replay_until.is_some_and(|t| t <= now) {
                // replay over: drop the extra delay, the shrink cuts back to the normal delay
                self.replay_until = None;
                self.replay_extra = Duration::ZERO;
                self.target = self.base_target;
            }
            if self.target < self.eff {
                self.try_shrink(now);
            }
            self.prepare_grow(now);
            if self.gap_fill_pending && self.queue.front().is_none_or(|h| h.cut_before && h.arrival + self.eff > now) {
                // everything before a censored gap is out: hold the last frame over the gap
                self.gap_fill_pending = false;
                let key = self.last_key_sent.clone();
                self.start_fill_with(now, key);
                continue;
            }
            while let Some(head) = self.queue.front() {
                if head.arrival + self.eff > now {
                    return;
                }
                if head.cut_before && self.gap_fill_pending {
                    break;
                }
                let can_freeze = (head.kind == Kind::Video && head.key) || !self.has_video;
                if self.target > self.eff && can_freeze && self.freeze_allowed() {
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
            target_ms: self.base_target.as_millis() as u64,
            current_ms: current.unwrap_or(self.eff).as_millis() as u64,
            buffered_ms: buffered.as_millis() as u64,
            buffered_bytes: self.queued_bytes,
            replaying: self.replay_until.is_some(),
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
            if q.cut_before {
                self.rewind_floor = Some(q.arrival);
            }
            if self.rebase_next || q.cut_before {
                self.offset = self.cut_base(cts) as i64 - q.ts as i64;
                self.rebase_next = false;
            }
            (q.ts as i64 + self.offset).max(0) as u64
        };
        self.last_sent_arrival = Some(q.arrival);
        if q.key {
            self.last_key_sent = Some(q.data.clone());
        }
        if !q.header && !self.history_window.is_zero() {
            self.history.push_back(q.clone());
        }
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

    fn trim_history(&mut self, now: Instant) {
        while let Some(h) = self.history.front() {
            if h.arrival + self.history_window >= now {
                break;
            }
            self.history.pop_front();
        }
    }

    /// Mode specific work before the delay can grow.
    fn prepare_grow(&mut self, now: Instant) {
        if self.target <= self.eff {
            self.rewound_for = None;
            if matches!(
                self.scene,
                SceneState::Requested { .. } | SceneState::Capturing { .. } | SceneState::Captured(_)
            ) {
                self.scene_events.push(SceneEvent::Back);
            }
            self.scene = SceneState::Idle;
            return;
        }
        match self.grow_mode {
            GrowMode::Rewind if self.rewound_for != Some(self.target) => {
                self.rewound_for = Some(self.target);
                self.try_rewind(now);
            }
            GrowMode::Scene => match self.scene {
                SceneState::Idle => {
                    self.scene = SceneState::Requested { since: now };
                    self.scene_events.push(SceneEvent::Show);
                }
                SceneState::Requested { since } | SceneState::Capturing { since, .. }
                    if now.saturating_duration_since(since) > SCENE_TIMEOUT =>
                {
                    log::warn!("delay scene did not show up in time, freezing instead");
                    self.scene = SceneState::Failed;
                    self.scene_events.push(SceneEvent::Back);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn freeze_allowed(&self) -> bool {
        match self.grow_mode {
            GrowMode::Scene => matches!(self.scene, SceneState::Captured(_) | SceneState::Failed),
            _ => true,
        }
    }

    /// Moves recently sent packets back into the queue so they are played again.
    /// Whatever history cannot cover is built up with a freeze afterwards.
    fn try_rewind(&mut self, now: Instant) {
        let want = now.checked_sub(self.target);
        let mut pick = None;
        for (i, h) in self.history.iter().enumerate() {
            if !h.key || self.rewind_floor.is_some_and(|f| h.arrival < f) {
                continue;
            }
            if pick.is_none() || want.is_some_and(|w| h.arrival <= w) {
                pick = Some(i);
            }
        }
        let Some(i) = pick else { return };
        let k_arrival = self.history[i].arrival;
        if self.last_sent_arrival.is_some_and(|last| k_arrival >= last) {
            return;
        }
        let replay: Vec<Queued> = self.history.drain(i..).collect();
        for q in replay.into_iter().rev() {
            self.queued_bytes += q.data.len();
            self.queue.push_front(q);
        }
        self.rebase_next = true;
        self.eff = now.saturating_duration_since(k_arrival).min(self.target);
        log::info!("rewound {:.1}s", self.eff.as_secs_f64());
    }

    fn start_fill(&mut self, now: Instant) {
        let head = self.queue.front().unwrap();
        let mut key = (head.kind == Kind::Video).then(|| head.data.clone());
        if let SceneState::Captured(k) = std::mem::replace(&mut self.scene, SceneState::Idle) {
            key = Some(k);
        }
        self.start_fill_with(now, key);
    }

    fn start_fill_with(&mut self, now: Instant, key: Option<Bytes>) {
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
            if !self.input_ended {
                self.fill = Some(f);
            }
            return false;
        };
        if head.arrival + self.target > now {
            self.fill = Some(f);
            return false;
        }
        // The buffer now holds the target delay: resume from the frozen keyframe.
        self.eff = now.saturating_duration_since(head.arrival).max(self.target);
        if let Some(h) = self.queue.front_mut() {
            h.cut_before = false; // the fill already rebased the timeline
        }
        let head = self.queue.front().unwrap();
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
            if dropped {
                self.rewind_floor = self.queue.front().map(|h| h.arrival);
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
            self.rewind_floor = self.queue.iter().find(|q| !q.header).map(|q| q.arrival);
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
                    let mut data = if key { vec![0x17, 1, 0, 0, 0] } else { vec![0x27, 1, 0, 0, 0] };
                    data.extend_from_slice(&(self.frame as u32).to_be_bytes());
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
        assert!((last_video - s.now_ms as i64).abs() < 300, "video ts {last_video} vs wall time {}", s.now_ms);

        s.engine.set_target(Duration::ZERO);
        s.run(2500);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Live, "{st:?}");
        assert!(st.current_ms < 2100, "{st:?}");
        assert!(st.buffered_ms < 2100, "{st:?}");
        s.assert_monotonic();
    }

    fn frame_of(p: &OutPacket) -> u32 {
        u32::from_be_bytes(p.data[5..9].try_into().unwrap())
    }

    #[test]
    fn rewind_is_instant_and_replays_history() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(13));
        s.run(15_000);
        let before = s.out.len();
        s.engine.set_target(Duration::from_secs(10));
        s.run(50);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Delayed, "{st:?}");
        assert!((9_900..=10_200).contains(&st.current_ms), "{st:?}");
        // the next video frame sent is an old keyframe (about 10 s back), not a filler
        let replay = s.out[before..].iter().find(|p| p.kind == Kind::Video).unwrap();
        assert_eq!(replay.data[0], 0x17);
        let f = frame_of(replay);
        assert!((120..=180).contains(&f), "replayed frame {f}");
        s.run(3000);
        s.assert_monotonic();
    }

    /// Largest step between consecutive output timestamps of a track.
    fn max_step(out: &[OutPacket], kind: Kind) -> u32 {
        let ts: Vec<u32> = out.iter().filter(|p| p.kind == kind).map(|p| p.ts).collect();
        ts.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0)
    }

    #[test]
    fn rewind_never_replays_across_a_cut() {
        // delay on, off (cut back to live), on again: the second rewind must not
        // reach into the part that was cut, or the timeline jumps by the cut
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(25));
        s.run(20_000);
        s.engine.set_target(Duration::from_secs(10));
        s.run(12_000);
        s.engine.set_target(Duration::ZERO);
        s.run(8_000);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Live);
        let before = s.out.len();
        s.engine.set_target(Duration::from_secs(10));
        s.run(15_000);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Delayed, "{st:?}");
        s.assert_monotonic();
        let video = max_step(&s.out[before..], Kind::Video);
        let audio = max_step(&s.out[before..], Kind::Audio);
        // a freeze step (500 ms at 2 fps) is fine, a hole of seconds is not
        assert!(video <= 600, "video timeline jumped {video} ms");
        assert!(audio <= 200, "audio timeline jumped {audio} ms");
    }

    #[test]
    fn rewind_with_short_history_freezes_the_rest() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(13));
        s.run(4_000);
        s.engine.set_target(Duration::from_secs(10));
        s.run(50);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Filling);
        s.run(8_000);
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Delayed, "{st:?}");
        assert!((9_900..=10_200).contains(&st.current_ms), "{st:?}");
        s.assert_monotonic();
    }

    #[test]
    fn scene_mode_freezes_on_frame_after_scene_switch() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Scene, Duration::ZERO);
        s.run(3_000);
        s.engine.set_target(Duration::from_secs(5));
        s.run(10);
        assert_eq!(s.engine.take_scene_events(), vec![SceneEvent::Show]);
        // OBS takes a moment to switch; output keeps flowing live meanwhile
        s.run(300);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Growing);
        s.engine.scene_shown(s.now());
        let shown_ms = s.now_ms;
        // the switch settles at +700 ms; the next keyframe (every 2 s) becomes the frozen picture
        s.run(3_000);
        assert_eq!(s.engine.take_scene_events(), vec![SceneEvent::Back]);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Filling);
        // the filler repeats the first keyframe encoded after the switch settled
        let filler = s.out.iter().rev().find(|p| p.kind == Kind::Video).unwrap();
        let f = frame_of(filler) as u64;
        assert!(f * 1000 / 30 >= shown_ms + 700, "froze on frame {f} before the scene switch");
        assert_eq!(f % 60, 0);
        s.run(6_000);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Delayed);
        s.assert_monotonic();
    }

    #[test]
    fn scene_mode_falls_back_to_freeze() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Scene, Duration::ZERO);
        s.run(3_000);
        s.engine.set_target(Duration::from_secs(5));
        s.run(10);
        s.engine.scene_unavailable();
        s.run(2_100);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Filling);
    }

    fn out_frames(p: &[OutPacket]) -> Vec<u32> {
        p.iter().filter(|p| p.kind == Kind::Video).map(frame_of).collect()
    }

    #[test]
    fn censor_removes_unaired_seconds() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(20));
        s.run(12_000);
        s.engine.set_target(Duration::from_secs(10));
        s.run(100);
        assert_eq!(s.engine.status(s.now()).phase, Phase::Delayed);
        // at 12.1 s viewers see ~2.1 s; remove the last 4 s of input (8.1 s .. 12.1 s)
        let removed = s.engine.censor(s.now(), Duration::from_secs(4));
        assert!((3_900..=4_100).contains(&(removed.as_millis() as u64)), "{removed:?}");
        let before = s.out.len();
        s.run(15_000);
        let frames = out_frames(&s.out[before..]);
        // nothing from the censored range (frames 243..363) ever airs
        let aired_censored: Vec<_> = frames.iter().filter(|f| (245..=360).contains(*f)).collect();
        assert!(aired_censored.is_empty(), "censored frames aired: {aired_censored:?}");
        // the delay is kept
        let st = s.engine.status(s.now());
        assert_eq!(st.phase, Phase::Delayed, "{st:?}");
        assert!((9_800..=10_300).contains(&st.current_ms), "{st:?}");
        s.assert_monotonic();
    }

    #[test]
    fn censor_needs_a_delay() {
        let mut s = Sim::new();
        s.run(5_000);
        assert_eq!(s.engine.censor(s.now(), Duration::from_secs(4)), Duration::ZERO);
    }

    #[test]
    fn replay_then_back_to_normal_delay() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Freeze, Duration::from_secs(20));
        s.run(15_000);
        let before = s.out.len();
        let got = s.engine.replay(s.now(), Duration::from_secs(8));
        assert!(got >= Duration::from_secs(7), "{got:?}");
        s.run(100);
        assert!(s.engine.status(s.now()).replaying);
        // the next frames are from ~8 s ago
        let first = out_frames(&s.out[before..])[0];
        assert!((180..=240).contains(&first), "replay started at frame {first}");
        s.run(got.as_millis() as u64 + 2_500);
        let st = s.engine.status(s.now());
        assert!(!st.replaying);
        assert_eq!(st.phase, Phase::Live, "{st:?}");
        assert!(st.current_ms < 2_100, "{st:?}");
        s.assert_monotonic();
    }

    #[test]
    fn replay_after_censor_stays_active() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(48));
        s.run(4_000);
        s.engine.set_target(Duration::from_secs(8));
        s.run(11_000);
        s.engine.censor(s.now(), Duration::from_secs(4));
        s.run(11_000);
        assert!(!s.engine.is_adjusting(), "{:?}", s.engine.status(s.now()));
        let got = s.engine.replay(s.now(), Duration::from_secs(5));
        assert!(got >= Duration::from_secs(4), "{got:?}");
        for step in 0..8 {
            s.run(200);
            assert!(s.engine.status(s.now()).replaying, "replay ended early at step {step}: {:?}", s.engine.status(s.now()));
        }
    }

    #[test]
    fn snapshot_has_recent_input_from_a_keyframe() {
        let mut s = Sim::new();
        s.engine.set_grow_mode(GrowMode::Rewind, Duration::from_secs(40));
        s.run(20_000);
        s.engine.set_target(Duration::from_secs(10));
        s.run(5_000);
        let clip = s.engine.snapshot(s.now(), Duration::from_secs(15), false).unwrap();
        assert!(clip.video_header.is_some() && clip.audio_header.is_some());
        let (k, ts, data) = &clip.packets[0];
        assert_eq!(*k, Kind::Video);
        assert_eq!(data[0], 0x17, "clip must start at a keyframe");
        assert!((8_000..=10_000).contains(ts), "starts at {ts}");
        let last = clip.packets.iter().filter(|p| p.0 == Kind::Video).map(|p| p.1).max().unwrap();
        assert!(last >= 24_900, "clip ends at {last}, should include unaired input");
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
