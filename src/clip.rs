//! Writes clips of the recent stream to disk: MP4 for H.264 + AAC (plays
//! everywhere, ready for TikTok/Shorts), FLV for anything else.

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

/// Saves `clip` into `dir` and returns the file path.
pub fn save(clip: &Clip, dir: &Path, size: (u16, u16)) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S").to_string();
    let avc = clip.video_header.as_deref().and_then(parse_avc_config);
    let aac = clip.audio_header.as_deref().and_then(parse_aac);
    if let Some(avc) = avc {
        let path = dir.join(format!("clip_{stamp}.mp4"));
        write_mp4(clip, &path, avc, aac, size)?;
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

fn write_mp4(clip: &Clip, path: &Path, avc: Avc, aac: Option<AacConfig>, (w, h): (u16, u16)) -> Result<()> {
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
        timescale: 1000,
        language: "und".into(),
        media_conf: MediaConfig::AvcConfig(AvcConfig { width: w, height: h, seq_param_set: avc.sps, pic_param_set: avc.pps }),
    })?;
    let has_audio = aac.is_some();
    if let Some(aac) = aac {
        mp4.add_track(&TrackConfig {
            track_type: TrackType::Audio,
            timescale: 1000,
            language: "und".into(),
            media_conf: MediaConfig::AacConfig(aac),
        })?;
    }

    let t0 = clip.packets.first().map_or(0, |p| p.1);
    let video: Vec<&(Kind, u64, Bytes)> =
        clip.packets.iter().filter(|p| p.0 == Kind::Video && p.2.len() > 5 && p.2[1] == 1).collect();
    let audio: Vec<&(Kind, u64, Bytes)> =
        clip.packets.iter().filter(|p| p.0 == Kind::Audio && p.2.len() > 2 && p.2[1] == 1).collect();
    if video.is_empty() {
        bail!("no video in the clip");
    }
    write_track(&mut mp4, 1, &video, t0, |d| (d.slice(5..), crate::flv::video_cts(d) as i32, d[0] >> 4 == 1))?;
    if has_audio && !audio.is_empty() {
        write_track(&mut mp4, 2, &audio, t0, |d| (d.slice(2..), 0, true))?;
    }
    mp4.write_end()?;
    mp4.into_writer().flush()?;
    Ok(())
}

fn write_track<W: Write + std::io::Seek>(
    mp4: &mut Mp4Writer<W>,
    track: u32,
    packets: &[&(Kind, u64, Bytes)],
    t0: u64,
    split: impl Fn(&Bytes) -> (Bytes, i32, bool),
) -> Result<()> {
    for (i, p) in packets.iter().enumerate() {
        let start = p.1.saturating_sub(t0);
        let next = packets.get(i + 1).map_or(p.1 + default_duration(packets), |n| n.1);
        let (bytes, rendering_offset, is_sync) = split(&p.2);
        mp4.write_sample(
            track,
            &Mp4Sample { start_time: start, duration: next.saturating_sub(p.1).max(1) as u32, rendering_offset, is_sync, bytes },
        )?;
    }
    Ok(())
}

fn default_duration(packets: &[&(Kind, u64, Bytes)]) -> u64 {
    match packets {
        [.., a, b] => b.1.saturating_sub(a.1).max(1),
        _ => 33,
    }
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
