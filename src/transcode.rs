//! A destination with a video bitrate of its own (YouTube at 15000 kbps, Twitch at
//! 6000): OBS encodes once, at the highest bitrate, and this stage re-encodes the
//! video for that destination with ffmpeg, on the graphics card when there is one.
//! It sits between the engine and the destination's [`upstream`](crate::upstream)
//! session, after the delay, so the delay, replay and "delete before it airs" work
//! exactly as for the others. Only the video goes through ffmpeg (timestamps kept);
//! the audio, the Twitch VOD track included, is passed on untouched and interleaved
//! back with the encoded video. If ffmpeg is missing or keeps failing, the stream is
//! sent as OBS sends it, with the reason in the destination's status.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::OutPacket;
use crate::flv::{self, Kind};
use crate::status::Shared;
use crate::t;
use crate::upstream::UpMsg;

/// An H.264 encoder ffmpeg can use, best first.
struct Encoder {
    /// Shown in the panel.
    label: &'static str,
    codec: &'static str,
    args: &'static [&'static str],
}

const ENCODERS: &[Encoder] = &[
    Encoder { label: "NVENC", codec: "h264_nvenc", args: &["-preset", "p5", "-tune", "hq", "-rc", "cbr"] },
    Encoder { label: "AMF", codec: "h264_amf", args: &["-usage", "transcoding", "-quality", "balanced", "-rc", "cbr"] },
    Encoder { label: "Quick Sync", codec: "h264_qsv", args: &["-preset", "medium"] },
    Encoder { label: "x264", codec: "libx264", args: &["-preset", "veryfast", "-x264-params", "nal-hrd=cbr"] },
];

/// Audio is held back for the video it goes with; with no video coming out of
/// ffmpeg for this long it goes on alone.
const AUDIO_WAIT: Duration = Duration::from_secs(5);
/// ffmpeg failures in a row before giving up and sending the stream as it comes.
const MAX_FAILURES: u32 = 3;

/// ffmpeg next to the program (shipped with the Setup), else the one on the PATH.
fn ffmpeg_path() -> PathBuf {
    if let Some(p) = std::env::var_os("DD_FFMPEG") {
        return p.into();
    }
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    match std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name))) {
        Some(p) if p.exists() => p,
        _ => name.into(),
    }
}

fn command(path: &PathBuf) -> Command {
    let mut c = Command::new(path);
    c.kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up while live
    c
}

/// The first encoder that works on this PC, found once (a short test encode each).
async fn encoder() -> Result<&'static Encoder, String> {
    static PICK: OnceCell<Result<usize, String>> = OnceCell::const_new();
    let pick = PICK
        .get_or_init(|| async {
            let path = ffmpeg_path();
            for (i, e) in ENCODERS.iter().enumerate() {
                let mut c = command(&path);
                c.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=1280x720:r=30", "-frames:v", "10", "-c:v", e.codec]);
                c.args(e.args).args(["-b:v", "2000k", "-f", "null", "-"]);
                match tokio::time::timeout(Duration::from_secs(15), c.status()).await {
                    Ok(Ok(st)) if st.success() => {
                        log::info!("video re-encoding with {} ({})", e.label, e.codec);
                        return Ok(i);
                    }
                    Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                        return Err(t!(
                            "ffmpeg not found: reinstall Dynamic Delay",
                            "ffmpeg não encontrado: reinstale o Dynamic Delay",
                            "no se encontró ffmpeg: reinstala Dynamic Delay"
                        ));
                    }
                    _ => {}
                }
            }
            Err(t!("no H.264 encoder works on this PC", "nenhum encoder H.264 funciona neste PC", "ningún codificador H.264 funciona en este PC"))
        })
        .await;
    pick.as_ref().map(|&i| &ENCODERS[i]).map_err(Clone::clone)
}

/// Starts the re-encoding stage in front of `up`; returns where the engine sends to.
pub fn spawn(shared: Arc<Shared>, id: u64, name: String, kbps: u32, up: UnboundedSender<UpMsg>) -> UnboundedSender<UpMsg> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(shared, id, name, kbps, rx, up));
    tx
}

/// What the ffmpeg tasks report, tagged with the run they belong to.
enum FfOut {
    Video(u64, OutPacket),
    Log(u64, String),
    Ended(u64),
}

struct Ffmpeg {
    run: u64,
    stdin: UnboundedSender<Bytes>,
    /// Frames are only fed from a keyframe on (after the codec header).
    wait_key: bool,
    _child: Child,
}

/// FLV file header for a video-only stream, plus PreviousTagSize0.
const FLV_HEADER: [u8; 13] = [b'F', b'L', b'V', 1, 0x01, 0, 0, 0, 9, 0, 0, 0, 0];

fn flv_tag(kind: u8, ts: u32, data: &[u8]) -> Bytes {
    let n = data.len() as u32;
    let mut v = Vec::with_capacity(data.len() + 15);
    v.push(kind);
    v.extend_from_slice(&n.to_be_bytes()[1..]);
    v.extend_from_slice(&ts.to_be_bytes()[1..]);
    v.push((ts >> 24) as u8);
    v.extend_from_slice(&[0, 0, 0]);
    v.extend_from_slice(data);
    v.extend_from_slice(&(n + 11).to_be_bytes());
    Bytes::from(v)
}

/// onMetaData with the frame rate and size: the encoder's rate control spreads the
/// bitrate over the frame rate, which ffmpeg cannot tell from a pipe on its own (a
/// wrong guess of 25 for 30 fps gives 20% more than asked).
fn metadata_tag(fps: f64, width: u32, height: u32) -> Bytes {
    let mut v = vec![2, 0, 10];
    v.extend_from_slice(b"onMetaData");
    v.push(8);
    v.extend_from_slice(&3u32.to_be_bytes());
    for (k, n) in [("framerate", fps), ("width", width as f64), ("height", height as f64)] {
        v.extend_from_slice(&(k.len() as u16).to_be_bytes());
        v.extend_from_slice(k.as_bytes());
        v.push(0);
        v.extend_from_slice(&n.to_be_bytes());
    }
    v.extend_from_slice(&[0, 0, 9]);
    flv_tag(18, 0, &v)
}

/// Reads the FLV tags ffmpeg writes and passes on the video ones.
async fn read_flv(mut r: impl AsyncReadExt + Unpin, run: u64, tx: &UnboundedSender<FfOut>) -> std::io::Result<()> {
    let mut head = [0u8; 13];
    r.read_exact(&mut head).await?;
    let mut tag = [0u8; 11];
    loop {
        r.read_exact(&mut tag).await?;
        let n = u32::from_be_bytes([0, tag[1], tag[2], tag[3]]) as usize;
        let ts = u32::from_be_bytes([tag[7], tag[4], tag[5], tag[6]]);
        let mut data = vec![0u8; n + 4];
        r.read_exact(&mut data).await?;
        data.truncate(n);
        if tag[0] & 0x1F == 9 {
            let _ = tx.send(FfOut::Video(run, OutPacket { kind: Kind::Video, ts, data: data.into() }));
        }
    }
}

struct Stage {
    shared: Arc<Shared>,
    id: u64,
    name: String,
    kbps: u32,
    up: UnboundedSender<UpMsg>,
    ff_tx: UnboundedSender<FfOut>,
    ff: Option<Ffmpeg>,
    runs: u64,
    failures: u32,
    /// Gave up on ffmpeg: the stream goes on as OBS sends it.
    passthrough: bool,
    /// Latest decoder config from OBS, fed to every new ffmpeg run.
    video_header: Option<OutPacket>,
    /// Frame rate and size OBS announced.
    video_info: Option<(f64, u32, u32)>,
    audio: VecDeque<(Instant, OutPacket)>,
    /// When ffmpeg last gave a frame.
    last_video_at: Option<Instant>,
    last_error: String,
}

impl Stage {
    fn set_status(&self, encoder: &str, error: Option<String>) {
        let mut st = self.shared.status.lock().unwrap();
        if let Some(o) = st.outputs.iter_mut().find(|o| o.id == self.id) {
            o.encoder = encoder.to_string();
            o.transcode_error = error;
        }
    }

    async fn start(&mut self) -> bool {
        let enc = match encoder().await {
            Ok(e) => e,
            Err(e) => {
                self.give_up(e);
                return false;
            }
        };
        let k = format!("{}k", self.kbps);
        let buf = format!("{}k", self.kbps * 2);
        let mut c = command(&ffmpeg_path());
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        c.args(["-hide_banner", "-loglevel", "error", "-fflags", "+nobuffer", "-probesize", "32", "-analyzeduration", "0"]);
        c.args(["-f", "flv", "-i", "pipe:0", "-map", "0:v:0", "-c:v", enc.codec]).args(enc.args);
        // keyframes where OBS put them (the delay cuts there), timestamps unchanged, no B-frames
        c.args(["-b:v", &k, "-maxrate", &k, "-bufsize", &buf, "-bf", "0", "-force_key_frames", "source"]);
        c.args(["-fps_mode", "passthrough", "-copyts", "-an", "-flush_packets", "1", "-f", "flv", "-flvflags", "no_duration_filesize", "pipe:1"]);
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.give_up(format!("ffmpeg: {e}"));
                return false;
            }
        };
        self.runs += 1;
        let run = self.runs;
        let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            return false;
        };
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Bytes>();
        tokio::spawn(async move {
            while let Some(b) = in_rx.recv().await {
                if stdin.write_all(&b).await.is_err() {
                    return;
                }
            }
            // closing stdin lets ffmpeg flush and exit
        });
        let tx = self.ff_tx.clone();
        tokio::spawn(async move {
            let _ = read_flv(stdout, run, &tx).await;
            let _ = tx.send(FfOut::Ended(run));
        });
        let tx = self.ff_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(FfOut::Log(run, l));
            }
        });
        let _ = in_tx.send(Bytes::from_static(&FLV_HEADER));
        if let Some((fps, w, h)) = self.video_info {
            let _ = in_tx.send(metadata_tag(fps, w, h));
        }
        if let Some(h) = &self.video_header {
            let _ = in_tx.send(flv_tag(9, h.ts, &h.data));
        }
        log::info!("[{}] re-encoding the video at {} kbps with {}", self.name, self.kbps, enc.label);
        self.set_status(enc.label, None);
        self.ff = Some(Ffmpeg { run, stdin: in_tx, wait_key: true, _child: child });
        true
    }

    /// Sends the stream on as OBS sends it from here on.
    fn give_up(&mut self, why: String) {
        log::warn!("[{}] video not re-encoded, sent as OBS sends it: {why}", self.name);
        self.passthrough = true;
        self.ff = None;
        self.flush_audio(None);
        // the platform got ffmpeg's decoder config: switch back to OBS' before its frames
        if let Some(h) = &self.video_header {
            let _ = self.up.send(UpMsg::Packet(h.clone()));
        }
        self.set_status("", Some(why));
    }

    fn video_in(&mut self, p: OutPacket) {
        if flv::video_is_sequence_header(&p.data) {
            self.video_header = Some(p.clone());
        }
        if self.passthrough {
            let _ = self.up.send(UpMsg::Packet(p));
            return;
        }
        let Some(ff) = &mut self.ff else { return };
        if flv::video_is_sequence_header(&p.data) {
            let _ = ff.stdin.send(flv_tag(9, p.ts, &p.data));
            return;
        }
        if ff.wait_key {
            if !flv::video_is_keyframe(&p.data) {
                return;
            }
            ff.wait_key = false;
        }
        let _ = ff.stdin.send(flv_tag(9, p.ts, &p.data));
    }

    /// A frame from ffmpeg. The destination needs one timeline across audio and
    /// video, so the audio up to this frame goes first.
    fn video_out(&mut self, p: OutPacket) {
        if !flv::video_is_sequence_header(&p.data) {
            self.failures = 0;
            self.last_video_at = Some(Instant::now());
        }
        self.flush_audio(Some(p.ts));
        let _ = self.up.send(UpMsg::Packet(p));
    }

    /// Passes on the audio up to the video sent (`None`: all of it).
    fn flush_audio(&mut self, upto: Option<u32>) {
        while let Some((_, a)) = self.audio.front() {
            if upto.is_some_and(|t| a.ts > t) {
                break;
            }
            let (_, a) = self.audio.pop_front().unwrap();
            let _ = self.up.send(UpMsg::Packet(a));
        }
    }

    /// With no video coming out for a while, the audio that waited goes on anyway.
    fn flush_stale(&mut self, now: Instant) {
        if self.last_video_at.is_some_and(|t| now.duration_since(t) < AUDIO_WAIT) {
            return;
        }
        while self.audio.front().is_some_and(|(at, _)| now.duration_since(*at) > AUDIO_WAIT) {
            let (_, a) = self.audio.pop_front().unwrap();
            let _ = self.up.send(UpMsg::Packet(a));
        }
    }
}

async fn run(shared: Arc<Shared>, id: u64, name: String, kbps: u32, mut rx: UnboundedReceiver<UpMsg>, up: UnboundedSender<UpMsg>) {
    let (ff_tx, mut ff_rx) = mpsc::unbounded_channel();
    let mut s = Stage {
        shared,
        id,
        name,
        kbps,
        up,
        ff_tx,
        ff: None,
        runs: 0,
        failures: 0,
        passthrough: false,
        video_header: None,
        video_info: None,
        audio: VecDeque::new(),
        last_video_at: None,
        last_error: String::new(),
    };
    // a failed run is restarted after a pause, at the next keyframe
    let mut retry_at: Option<Instant> = Some(Instant::now());
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                None => return,
                Some(UpMsg::Stop) => break,
                Some(UpMsg::CatchUp) => { let _ = s.up.send(UpMsg::CatchUp); }
                Some(UpMsg::Metadata(mut m)) => {
                    if let Some(fps) = m.video_frame_rate.filter(|f| *f > 0.0) {
                        s.video_info = Some((fps as f64, m.video_width.unwrap_or(0), m.video_height.unwrap_or(0)));
                    }
                    if !s.passthrough {
                        m.video_bitrate_kbps = Some(s.kbps);
                        m.video_codec_id = Some(7); // H.264
                    }
                    let _ = s.up.send(UpMsg::Metadata(m));
                }
                Some(UpMsg::Packet(p)) if p.kind == Kind::Audio => {
                    if s.passthrough {
                        let _ = s.up.send(UpMsg::Packet(p));
                    } else {
                        s.audio.push_back((Instant::now(), p));
                    }
                }
                Some(UpMsg::Packet(p)) => {
                    if s.ff.is_none() && !s.passthrough && retry_at.is_some_and(|t| Instant::now() >= t) && flv::video_is_keyframe(&p.data) && !flv::video_is_sequence_header(&p.data) {
                        retry_at = None;
                        s.start().await;
                    }
                    s.video_in(p);
                }
            },
            o = ff_rx.recv() => match o {
                Some(FfOut::Video(run, p)) if s.ff.as_ref().is_some_and(|f| f.run == run) => s.video_out(p),
                Some(FfOut::Log(run, l)) => {
                    if s.ff.as_ref().is_some_and(|f| f.run == run) {
                        log::warn!("[{}] ffmpeg: {l}", s.name);
                        s.last_error = l;
                    }
                }
                Some(FfOut::Ended(run)) if s.ff.as_ref().is_some_and(|f| f.run == run) => {
                    s.ff = None;
                    s.failures += 1;
                    let why = if s.last_error.is_empty() { "ffmpeg stopped".to_string() } else { format!("ffmpeg: {}", s.last_error) };
                    if s.failures >= MAX_FAILURES {
                        s.give_up(why);
                    } else {
                        log::warn!("[{}] {why}; restarting the re-encoding", s.name);
                        retry_at = Some(Instant::now() + Duration::from_secs(1));
                    }
                }
                _ => {}
            },
            _ = tick.tick() => s.flush_stale(Instant::now()),
        }
    }
    // end of the stream: let ffmpeg finish what it has, then stop the destination
    if let Some(ff) = s.ff.take() {
        let run = ff.run;
        drop(ff.stdin);
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                o = ff_rx.recv() => match o {
                    Some(FfOut::Video(r, p)) if r == run => s.video_out(p),
                    Some(FfOut::Ended(r)) if r == run => break,
                    None => break,
                    _ => {}
                },
                _ = &mut deadline => break,
            }
        }
    }
    s.flush_audio(None);
    let _ = s.up.send(UpMsg::Stop);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flv_tags_round_trip() {
        let t = flv_tag(9, 0x0102_0304, &[0x17, 1, 2]);
        assert_eq!(&t[..11], &[9, 0, 0, 3, 0x02, 0x03, 0x04, 0x01, 0, 0, 0]);
        assert_eq!(&t[11..14], &[0x17, 1, 2]);
        assert_eq!(&t[14..], &14u32.to_be_bytes());
        let mut file = FLV_HEADER.to_vec();
        file.extend_from_slice(&t);
        file.extend_from_slice(&flv_tag(8, 5, &[0xAF, 1]));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async { read_flv(&file[..], 1, &tx).await.unwrap_err() });
        match rx.try_recv() {
            Ok(FfOut::Video(1, p)) => assert_eq!((p.ts, &p.data[..]), (0x0102_0304, &[0x17u8, 1, 2][..])),
            _ => panic!("video tag not read"),
        }
        assert!(rx.try_recv().is_err(), "audio tags are not passed on");
    }
}
