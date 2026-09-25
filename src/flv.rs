//! Helpers to inspect FLV audio/video tag bodies as carried by RTMP.
//! Supports legacy FLV (AVC/AAC) and Enhanced RTMP (HEVC, AV1, ...) headers.

use bytes::Bytes;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Audio,
    Video,
}

impl Kind {
    pub fn idx(self) -> usize {
        match self {
            Kind::Audio => 0,
            Kind::Video => 1,
        }
    }
}

fn video_frame_type(d: &[u8]) -> Option<u8> {
    let b0 = *d.first()?;
    Some(if b0 & 0x80 != 0 { (b0 >> 4) & 0x07 } else { b0 >> 4 })
}

pub fn video_is_sequence_header(d: &[u8]) -> bool {
    let Some(&b0) = d.first() else { return false };
    if b0 & 0x80 != 0 {
        // Enhanced RTMP: PacketType 0 = SequenceStart
        return b0 & 0x0F == 0;
    }
    let codec = b0 & 0x0F;
    matches!(codec, 7 | 12 | 13) && d.get(1) == Some(&0)
}

pub fn video_is_keyframe(d: &[u8]) -> bool {
    video_frame_type(d) == Some(1) && !video_is_sequence_header(d)
}

/// Composition time offset (PTS - DTS) in ms of a coded video frame, 0 if absent.
/// A video tag that carries a picture (not a codec header or end-of-sequence).
pub fn is_video_frame(d: &[u8]) -> bool {
    match d.first() {
        // Enhanced RTMP: PacketType 1 (CodedFrames) or 3 (CodedFramesX)
        Some(b0) if b0 & 0x80 != 0 => matches!(b0 & 0x0F, 1 | 3),
        Some(_) => d.get(1) == Some(&1),
        None => false,
    }
}

pub fn video_cts(d: &[u8]) -> i64 {
    let Some(&b0) = d.first() else { return 0 };
    let at = if b0 & 0x80 != 0 {
        // Enhanced RTMP: only PacketType 1 (CodedFrames) carries it, after the FourCC.
        if b0 & 0x0F != 1 {
            return 0;
        }
        5
    } else if matches!(b0 & 0x0F, 7 | 12 | 13) {
        2
    } else {
        return 0;
    };
    match d.get(at..at + 3) {
        Some(b) => {
            let v = ((b[0] as i32) << 16) | ((b[1] as i32) << 8) | b[2] as i32;
            ((v << 8) >> 8) as i64 // sign-extend 24 bits
        }
        None => 0,
    }
}

pub fn audio_is_sequence_header(d: &[u8]) -> bool {
    let Some(&b0) = d.first() else { return false };
    match b0 >> 4 {
        10 => d.get(1) == Some(&0),
        9 => b0 & 0x0F == 0,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AacInfo {
    pub header_byte: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Parses an FLV AAC sequence header (AudioSpecificConfig).
pub fn parse_aac_config(d: &[u8]) -> Option<AacInfo> {
    if d.len() < 4 || d[0] >> 4 != 10 || d[1] != 0 {
        return None;
    }
    let asc = &d[2..];
    let object_type = asc[0] >> 3;
    let freq_idx = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
    let channels = (asc[1] >> 3) & 0x0F;
    if object_type != 2 || !(1..=2).contains(&channels) {
        return None;
    }
    Some(AacInfo {
        header_byte: d[0],
        sample_rate: *AAC_RATES.get(freq_idx as usize)?,
        channels,
    })
}

/// A raw AAC-LC frame (1024 samples) that decodes to digital silence.
pub fn silent_aac_frame(info: &AacInfo) -> Bytes {
    let raw: &[u8] = if info.channels == 1 {
        &[0x01, 0x40, 0x20, 0x07]
    } else {
        &[0x21, 0x10, 0x04, 0x60, 0x8C, 0x1C]
    };
    let mut v = Vec::with_capacity(raw.len() + 2);
    v.push(info.header_byte);
    v.push(1); // AACPacketType = raw
    v.extend_from_slice(raw);
    Bytes::from(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_avc() {
        assert!(video_is_sequence_header(&[0x17, 0x00, 0, 0, 0]));
        assert!(!video_is_keyframe(&[0x17, 0x00, 0, 0, 0]));
        assert!(video_is_keyframe(&[0x17, 0x01, 0, 0, 0]));
        assert!(!video_is_keyframe(&[0x27, 0x01, 0, 0, 0]));
        assert_eq!(video_cts(&[0x27, 0x01, 0, 0, 67]), 67);
        assert_eq!(video_cts(&[0x27, 0x01, 0xFF, 0xFF, 0xFF]), -1);
        assert_eq!(video_cts(&[0x91, b'h', b'v', b'c', b'1', 0, 1, 0]), 256);
    }

    #[test]
    fn detects_enhanced() {
        // IsExHeader | key frame | SequenceStart, fourcc hvc1
        assert!(video_is_sequence_header(&[0x90, b'h', b'v', b'c', b'1']));
        // IsExHeader | key frame | CodedFramesX
        assert!(video_is_keyframe(&[0x93, b'h', b'v', b'c', b'1']));
        assert!(!video_is_keyframe(&[0xA3, b'h', b'v', b'c', b'1']));
    }

    #[test]
    fn parses_aac() {
        // AAC LC, 48 kHz, stereo -> ASC 0x11 0x90
        let info = parse_aac_config(&[0xAF, 0x00, 0x11, 0x90]).unwrap();
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.channels, 2);
        assert!(audio_is_sequence_header(&[0xAF, 0x00, 0x11, 0x90]));
        assert!(!audio_is_sequence_header(&[0xAF, 0x01, 0x21]));
    }
}
