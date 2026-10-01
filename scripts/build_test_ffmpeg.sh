#!/usr/bin/env bash
# Ubuntu's FFmpeg 6 cannot mux the Enhanced FLV audio track. Build just the
# components used by e2e-vod-track.sh; the system FFmpeg generates the fixture.
set -euo pipefail
: "${DD_FFMPEG_PREFIX:?Set an absolute installation directory}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
curl --fail --show-error --location --retry 3 https://ffmpeg.org/releases/ffmpeg-8.0.1.tar.xz -o ffmpeg.tar.xz
echo '05ee0b03119b45c0bdb4df654b96802e909e0a752f72e4fe3794f487229e5a41  ffmpeg.tar.xz' | sha256sum --check
tar --no-same-owner -xf ffmpeg.tar.xz
cd ffmpeg-8.0.1
./configure --prefix="$DD_FFMPEG_PREFIX" \
  --disable-autodetect --disable-x86asm --disable-doc --disable-debug --disable-ffplay --disable-everything \
  --enable-protocol=file,pipe,rtmp,tcp --enable-demuxer=matroska,flv --enable-muxer=flv,null,pcm_s16le \
  --enable-decoder=h264,aac --enable-encoder=pcm_s16le --enable-filter=aresample,anull,aformat \
  --enable-parser=h264,aac --enable-bsf=aac_adtstoasc
make -j2
make install
