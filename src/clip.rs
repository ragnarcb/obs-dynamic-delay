//! Writes clips of the recent stream to disk: MP4 for H.264 + AAC (plays
//! everywhere, ready for TikTok/Shorts), FLV for anything else.
//!
//! Clips copy the stream frame by frame (no re-encoding), so they keep its
//! size and frame rate. RTMP timestamps are whole milliseconds (16/17 ms at
//! 60 FPS); the MP4 puts the frames on an exact grid of the frame rate so
//! editors see a constant frame rate.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use mp4::{
    AacConfig, AudioObjectType, AvcConfig, ChannelConfig, FourCC, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer,
    SampleFreqIndex, TrackConfig, TrackType,
};

use crate::engine::Clip;
use crate::flv::Kind;

/// Saves `clip` into `dir` and returns the file path. `fps` is the rate OBS
/// announced, if any (otherwise it is measured from the clip).
pub fn save(clip: &Clip, dir: &Path, size: (u16, u16), fps: Option<f64>) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S").to_string();
    let avc = clip.video_header.as_deref().and_then(parse_avc_config);
    let aac = clip.audio_header.as_deref().and_then(parse_aac);
    if let Some(avc) = avc {
        let path = dir.join(format!("clip_{stamp}.mp4"));
        write_mp4(clip, &path, avc, aac, size, fps)?;
        Ok(path)
    } else {
        let path = dir.join(format!("clip_{stamp}.flv"));
        write_flv(clip, &path)?;
        Ok(path)
    }
}

struct Avc {
    sps: Vec<u8>,
    pps: Vec<u8>,
}

/// Reads SPS/PPS from an FLV AVC sequence header (AVCDecoderConfigurationRecord).
fn parse_avc_config(d: &[u8]) -> Option<Avc> {
    if d.len() < 11 || d[0] & 0x0F != 7 || d[1] != 0 {
        return None;
    }
    let r = &d[5..];
    let mut i = 5; // version, profile, compat, level, length size
    let num_sps = (r.get(i)? & 0x1F) as usize;
    i += 1;
    let mut sps = None;
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*r.get(i)?, *r.get(i + 1)?]) as usize;
        sps.get_or_insert_with(|| r.get(i + 2..i + 2 + len).map(<[u8]>::to_vec));
        i += 2 + len;
    }
    let num_pps = *r.get(i)? as usize;
    i += 1;
    let mut pps = None;
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*r.get(i)?, *r.get(i + 1)?]) as usize;
        pps.get_or_insert_with(|| r.get(i + 2..i + 2 + len).map(<[u8]>::to_vec));
        i += 2 + len;
    }
    Some(Avc { sps: sps.flatten()?, pps: pps.flatten()? })
}

/// Picture size from an FLV AVC sequence header.
pub fn avc_size(header: &[u8]) -> Option<(u16, u16)> {
    let avc = parse_avc_config(header)?;
    sps_size(&avc.sps)
}

/// Bit reader over an H.264 NAL unit (emulation prevention bytes removed).
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(nal: &[u8]) -> Self {
        let mut data = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for &b in nal {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Bits { data, pos: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |acc, _| Some((acc << 1) | self.bit()?))
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let v = self.ue()?;
        Some(if v % 2 == 1 { v.div_ceil(2) as i32 } else { -((v / 2) as i32) })
    }
}

/// Width and height coded in an H.264 SPS (with the cropping applied).
fn sps_size(sps: &[u8]) -> Option<(u16, u16)> {
    let mut r = Bits::new(sps.get(1..)?); // skip the NAL header
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags, level
    r.ue()?; // seq_parameter_set_id
    let mut chroma = 1;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?; // separate_colour_plane_flag
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let w_mbs = r.ue()? + 1;
    let h_units = r.ue()? + 1;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.bit()?;
    }
    r.bit()?; // direct_8x8_inference_flag
    let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
    if r.bit()? == 1 {
        (cl, cr, ct, cb) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
    }
    let (sub_w, sub_h) = match chroma {
        0 => (1, 1),
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let unit_y = sub_h * (2 - frame_mbs_only);
    let width = (w_mbs * 16).checked_sub((cl + cr) * sub_w)?;
    let height = ((2 - frame_mbs_only) * h_units * 16).checked_sub((ct + cb) * unit_y)?;
    Some((u16::try_from(width).ok()?, u16::try_from(height).ok()?))
}

/// (timescale, ticks per frame) for a frame rate, snapped to the usual rates.
fn frame_grid(fps: f64) -> (u32, u32) {
    const RATES: [(f64, u32, u32); 11] = [
        (23.976, 24000, 1001),
        (24.0, 24000, 1000),
        (25.0, 25000, 1000),
        (29.97, 30000, 1001),
        (30.0, 30000, 1000),
        (48.0, 48000, 1000),
        (50.0, 50000, 1000),
        (59.94, 60000, 1001),
        (60.0, 60000, 1000),
        (100.0, 100000, 1000),
        (120.0, 120000, 1000),
    ];
    let best = RATES.iter().min_by(|a, b| (a.0 - fps).abs().total_cmp(&(b.0 - fps).abs())).unwrap();
    if (best.0 - fps).abs() / best.0 < 0.03 {
        (best.1, best.2)
    } else {
        // unusual rate: whole milliseconds on a fine clock
        (90_000, ((90_000.0 / fps.max(1.0)).round() as u32).max(1))
    }
}

/// Frame rate measured from the timestamps (ms) of the video frames.
fn measured_fps(video: &[&(Kind, u64, Bytes)]) -> Option<f64> {
    let (first, last) = (video.first()?.1, video.last()?.1);
    (video.len() > 10 && last > first).then(|| (video.len() - 1) as f64 * 1000.0 / (last - first) as f64)
}

fn parse_aac(d: &[u8]) -> Option<AacConfig> {
    let info = crate::flv::parse_aac_config(d)?;
    let asc = &d[2..];
    let freq = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
    Some(AacConfig {
        bitrate: 160_000,
        profile: AudioObjectType::AacLowComplexity,
        freq_index: SampleFreqIndex::try_from(freq).ok()?,
        chan_conf: ChannelConfig::try_from(info.channels).ok()?,
    })
}

fn write_mp4(clip: &Clip, path: &Path, avc: Avc, aac: Option<AacConfig>, size: (u16, u16), fps: Option<f64>) -> Result<()> {
    // the size written in the H.264 header wins over OBS' metadata
    let (w, h) = sps_size(&avc.sps).unwrap_or(size);
    let video: Vec<&(Kind, u64, Bytes)> =
        clip.packets.iter().filter(|p| p.0 == Kind::Video && p.2.len() > 5 && p.2[1] == 1).collect();
    let audio: Vec<&(Kind, u64, Bytes)> =
        clip.packets.iter().filter(|p| p.0 == Kind::Audio && p.2.len() > 2 && p.2[1] == 1).collect();
    if video.is_empty() {
        bail!("no video in the clip");
    }
    let fps = fps.filter(|f| *f >= 1.0 && *f <= 240.0).or_else(|| measured_fps(&video)).unwrap_or(30.0);
    let (v_scale, v_tick) = frame_grid(fps);
    let a_rate = aac.as_ref().map_or(48_000, |a| a.freq_index.freq());
    let file = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
    let config = Mp4Config {
        major_brand: "isom".parse()?,
        minor_version: 512,
        compatible_brands: ["isom", "iso2", "avc1", "mp41"].iter().map(|b| b.parse()).collect::<Result<Vec<FourCC>, _>>()?,
        timescale: 1000,
    };
    let mut mp4 = Mp4Writer::write_start(file, &config)?;
    mp4.add_track(&TrackConfig {
        track_type: TrackType::Video,
        timescale: v_scale,
        language: "und".into(),
        media_conf: MediaConfig::AvcConfig(AvcConfig { width: w, height: h, seq_param_set: avc.sps, pic_param_set: avc.pps }),
    })?;
    let has_audio = aac.is_some();
    if let Some(aac) = aac {
        mp4.add_track(&TrackConfig {
            track_type: TrackType::Audio,
            timescale: a_rate,
            language: "und".into(),
            media_conf: MediaConfig::AacConfig(aac),
        })?;
    }

    // one video frame / one AAC frame (1024 samples) per step of the grid
    let frame_ms = 1000.0 * v_tick as f64 / v_scale as f64;
    write_track(&mut mp4, 1, &video, frame_ms, v_tick, |d| (d.slice(5..), crate::flv::video_cts(d), d[0] >> 4 == 1))?;
    if has_audio && !audio.is_empty() {
        write_track(&mut mp4, 2, &audio, 1_024_000.0 / a_rate as f64, 1024, |d| (d.slice(2..), 0, true))?;
    }
    mp4.write_end()?;
    mp4.into_writer().flush()?;
    Ok(())
}

/// Writes the samples on a grid of `tick` units, one step every `step_ms`.
/// Gaps (dropped frames) become whole steps, so audio and video stay in sync.
fn write_track<W: Write + std::io::Seek>(
    mp4: &mut Mp4Writer<W>,
    track: u32,
    packets: &[&(Kind, u64, Bytes)],
    step_ms: f64,
    tick: u32,
    split: impl Fn(&Bytes) -> (Bytes, i64, bool),
) -> Result<()> {
    let mut at = 0u64; // in steps
    for (i, p) in packets.iter().enumerate() {
        let steps = packets.get(i + 1).map_or(1, |n| ((n.1.saturating_sub(p.1)) as f64 / step_ms).round().max(1.0) as u64);
        let (bytes, cts_ms, is_sync) = split(&p.2);
        let offset = (cts_ms as f64 / step_ms).round() as i32 * tick as i32;
        mp4.write_sample(
            track,
            &Mp4Sample { start_time: at * tick as u64, duration: (steps * tick as u64) as u32, rendering_offset: offset, is_sync, bytes },
        )?;
        at += steps;
    }
    Ok(())
}

fn write_flv(clip: &Clip, path: &Path) -> Result<()> {
    let mut f = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
    let has_audio = clip.audio_header.is_some();
    let flags = if has_audio { 0x05 } else { 0x01 };
    f.write_all(&[b'F', b'L', b'V', 1, flags, 0, 0, 0, 9, 0, 0, 0, 0])?;
    let t0 = clip.packets.first().map_or(0, |p| p.1);
    let tag = |f: &mut BufWriter<File>, kind: Kind, ts: u64, data: &[u8]| -> Result<()> {
        let ty = if kind == Kind::Audio { 8u8 } else { 9 };
        let len = data.len() as u32;
        let ts = ts as u32;
        f.write_all(&[ty, (len >> 16) as u8, (len >> 8) as u8, len as u8])?;
        f.write_all(&[(ts >> 16) as u8, (ts >> 8) as u8, ts as u8, (ts >> 24) as u8, 0, 0, 0])?;
        f.write_all(data)?;
        f.write_all(&(11 + len).to_be_bytes())?;
        Ok(())
    };
    if let Some(h) = &clip.video_header {
        tag(&mut f, Kind::Video, 0, h)?;
    }
    if let Some(h) = &clip.audio_header {
        tag(&mut f, Kind::Audio, 0, h)?;
    }
    for (kind, ts, data) in &clip.packets {
        tag(&mut f, *kind, ts.saturating_sub(t0), data)?;
    }
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_grid_snaps_to_common_rates() {
        assert_eq!(frame_grid(60.0), (60000, 1000));
        assert_eq!(frame_grid(60.007), (60000, 1000));
        assert_eq!(frame_grid(59.94), (60000, 1001));
        assert_eq!(frame_grid(30.0), (30000, 1000));
        assert_eq!(frame_grid(29.97), (30000, 1001));
        assert_eq!(frame_grid(144.0), (90000, 625));
    }

    #[test]
    fn sps_size_reads_1080p_with_cropping() {
        // High profile SPS of a 1920x1080 x264 stream (1088 coded, cropped to 1080)
        let sps = [
            0x67, 0x64, 0x00, 0x2a, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
            0x00, 0x03, 0x01, 0xe0, 0x3c, 0x60, 0xc6, 0x58,
        ];
        assert_eq!(sps_size(&sps), Some((1920, 1080)));
        let sps720 = [
            0x67, 0x64, 0x00, 0x20, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00,
            0x07, 0x80, 0xf1, 0x83, 0x19, 0x60,
        ];
        assert_eq!(sps_size(&sps720), Some((1280, 720)));
    }
}
