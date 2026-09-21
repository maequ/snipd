//! H.264 encoding to MP4, via Media Foundation.
//!
//! # Why Media Foundation
//!
//! It ships with Windows. Bundling ffmpeg would add tens of megabytes to a 5 MB
//! app and drag in licensing questions that an MIT-licensed project should not
//! have to answer. Media Foundation's H.264 encoder is hardware-accelerated on
//! most machines, which matters a great deal when the alternative is encoding
//! 30 full-screen frames a second on the CPU.
//!
//! # Letting the pipeline do the work
//!
//! Frames arrive as top-down RGB32 straight from the screen grab. Rather than
//! converting colour space and rescaling ourselves, the sink writer is given an
//! RGB32 *input* type and an H.264 *output* type at the target resolution, and
//! Media Foundation inserts the converter and scaler itself. That hands the work
//! to code that is already optimised for it, and keeps the capture loop to
//! grabbing pixels and handing them over.

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSinkWriter, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
    MFVideoInterlace_Progressive, MFSTARTUP_NOSOCKET, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION,
};

/// One hundred-nanosecond units per second, the unit Media Foundation counts in.
const UNITS_PER_SECOND: i64 = 10_000_000;

/// Sample rates the Windows AAC encoder accepts. Anything else it refuses
/// outright, and resampling is not worth doing inside a screenshot tool.
pub const SUPPORTED_AUDIO_RATES: [u32; 2] = [44_100, 48_000];

/// Bytes per second for the AAC stream. 24000 is 192 kbit/s, comfortably
/// transparent for system audio and negligible next to the video.
const AAC_BYTES_PER_SECOND: u32 = 24_000;

/// What the picture half of a recording looks like.
///
/// Grouped rather than passed as a handful of bare integers: six numbers in a
/// row, four of which are dimensions, is an easy thing to transpose by accident
/// and a hard mistake to spot afterwards.
#[derive(Debug, Clone, Copy)]
pub struct VideoSpec {
    /// Size of the frames handed to [`Encoder::write_frame`].
    pub source_width: u32,
    pub source_height: u32,
    /// Size actually encoded. This is how the resolution setting takes effect.
    pub target_width: u32,
    pub target_height: u32,
    pub fps: u32,
    pub bitrate_bits: u32,
}

/// What the audio half of a recording looks like.
#[derive(Debug, Clone, Copy)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Wraps a sink writer configured for one recording.
pub struct Encoder {
    writer: IMFSinkWriter,
    stream: u32,
    /// Present only when system audio is being recorded.
    audio_stream: Option<u32>,
    audio: Option<AudioSpec>,
    /// Audio frames handed over so far, which is what timestamps are derived
    /// from -- the same approach the video side uses, so the two stay aligned.
    audio_frames: u64,
    /// Capture dimensions, which are also the input frame dimensions.
    source_width: u32,
    source_height: u32,
    frame_duration: i64,
    frames: u64,
}

impl Encoder {
    /// Start a recording.
    pub fn create(
        output: &Path,
        video: VideoSpec,
        audio: Option<AudioSpec>,
    ) -> Result<Self, String> {
        let VideoSpec {
            source_width,
            source_height,
            fps,
            bitrate_bits,
            ..
        } = video;
        // H.264 requires even dimensions; an odd one is rejected outright with an
        // unhelpful error, so they are rounded down here instead.
        let target_width = video.target_width & !1;
        let target_height = video.target_height & !1;
        if target_width == 0 || target_height == 0 {
            return Err("the recording area is too small".into());
        }

        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)
                .map_err(|e| format!("Media Foundation could not start: {e}"))?;

            let path: Vec<u16> = output
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            let writer: IMFSinkWriter =
                MFCreateSinkWriterFromURL(PCWSTR(path.as_ptr()), None, None)
                    .map_err(|e| format!("could not create {}: {e}", output.display()))?;

            // --- what gets written ---
            let out_type: IMFMediaType =
                MFCreateMediaType().map_err(|e| format!("output media type: {e}"))?;
            out_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            out_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| e.to_string())?;
            out_type
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bits)
                .map_err(|e| e.to_string())?;
            out_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| e.to_string())?;
            // High profile rather than the encoder's default. It permits
            // 8x8 transforms and CABAC, both of which matter disproportionately
            // for screen content: sharp text edges are precisely what the
            // simpler profiles smear. Every Windows H.264 decoder supports it.
            const H264_PROFILE_HIGH: u32 = 100;
            out_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, H264_PROFILE_HIGH)
                .map_err(|e| format!("setting the H.264 profile: {e}"))?;
            set_size(&out_type, MF_MT_FRAME_SIZE, target_width, target_height)?;
            set_ratio(&out_type, MF_MT_FRAME_RATE, fps, 1)?;
            set_ratio(&out_type, MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;

            let stream = writer
                .AddStream(&out_type)
                .map_err(|e| format!("could not add a video stream: {e}"))?;

            // --- what we hand over ---
            let in_type: IMFMediaType =
                MFCreateMediaType().map_err(|e| format!("input media type: {e}"))?;
            in_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            in_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| e.to_string())?;
            in_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| e.to_string())?;
            // Without this the recording comes out upside down.
            //
            // Uncompressed RGB in Media Foundation is bottom-up by convention,
            // so a frame handed over with no stride declared is interpreted as
            // starting at the bottom row. The screen grab produces top-down
            // frames, which a *positive* stride declares. Getting this wrong is
            // not subtle: every frame is mirrored vertically.
            in_type
                .SetUINT32(&MF_MT_DEFAULT_STRIDE, source_width * 4)
                .map_err(|e| format!("setting the frame stride: {e}"))?;
            set_size(&in_type, MF_MT_FRAME_SIZE, source_width, source_height)?;
            set_ratio(&in_type, MF_MT_FRAME_RATE, fps, 1)?;
            set_ratio(&in_type, MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;

            writer
                .SetInputMediaType(stream, &in_type, None)
                .map_err(|e| {
                    format!("this machine has no encoder for {target_width}x{target_height}: {e}")
                })?;

            // --- audio, when there is any ---
            //
            // Added before BeginWriting, because a sink writer's streams are
            // fixed once writing has started. Audio failing is never fatal: a
            // recording with no sound beats no recording at all.
            let audio_stream = match audio {
                Some(spec) => match add_audio_stream(&writer, spec) {
                    Ok(index) => Some(index),
                    Err(err) => {
                        eprintln!("[record] recording without audio: {err}");
                        None
                    }
                },
                None => None,
            };

            writer
                .BeginWriting()
                .map_err(|e| format!("could not begin writing: {e}"))?;

            Ok(Self {
                writer,
                stream,
                audio_stream,
                audio: audio_stream.and(audio),
                audio_frames: 0,
                source_width,
                source_height,
                frame_duration: UNITS_PER_SECOND / fps.max(1) as i64,
                frames: 0,
            })
        }
    }

    /// Hand over one top-down BGRA frame.
    ///
    /// Frames are timestamped by index rather than by wall clock, so a recording
    /// always plays back at exactly the requested frame rate. If the capture loop
    /// falls behind, the result is a video that runs slightly fast rather than
    /// one whose audio-less timeline drifts — and the loop reports the shortfall
    /// separately so it is never silently hidden.
    pub fn write_frame(&mut self, bgra: &[u8]) -> Result<(), String> {
        let stride = self.source_width as usize * 4;
        let expected = stride * self.source_height as usize;
        if bgra.len() < expected {
            return Err(format!(
                "frame is {} bytes, expected {expected}",
                bgra.len()
            ));
        }

        unsafe {
            let buffer = MFCreateMemoryBuffer(expected as u32)
                .map_err(|e| format!("could not allocate a frame buffer: {e}"))?;

            let mut destination: *mut u8 = std::ptr::null_mut();
            buffer
                .Lock(&mut destination, None, None)
                .map_err(|e| format!("could not lock the frame buffer: {e}"))?;
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), destination, expected);
            buffer.Unlock().map_err(|e| e.to_string())?;
            buffer
                .SetCurrentLength(expected as u32)
                .map_err(|e| e.to_string())?;

            let sample = MFCreateSample().map_err(|e| format!("could not create a sample: {e}"))?;
            sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
            sample
                .SetSampleTime(self.frames as i64 * self.frame_duration)
                .map_err(|e| e.to_string())?;
            sample
                .SetSampleDuration(self.frame_duration)
                .map_err(|e| e.to_string())?;

            self.writer
                .WriteSample(self.stream, &sample)
                .map_err(|e| format!("could not write a frame: {e}"))?;
        }

        self.frames += 1;
        Ok(())
    }

    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    /// True when an audio stream was successfully configured.
    pub fn has_audio(&self) -> bool {
        self.audio_stream.is_some()
    }

    /// Hand over interleaved 16-bit PCM.
    ///
    /// Timestamps come from the running sample count rather than the clock, for
    /// the same reason the video side uses its frame index: the two then
    /// describe one consistent timeline, and audio cannot drift away from
    /// picture just because a block arrived late.
    pub fn write_audio(&mut self, pcm: &[u8]) -> Result<(), String> {
        let (Some(stream), Some(spec)) = (self.audio_stream, self.audio) else {
            return Ok(());
        };
        if pcm.is_empty() {
            return Ok(());
        }

        let bytes_per_frame = spec.channels as usize * 2;
        if bytes_per_frame == 0 {
            return Ok(());
        }
        let frames = pcm.len() / bytes_per_frame;
        if frames == 0 {
            return Ok(());
        }

        unsafe {
            let buffer = MFCreateMemoryBuffer(pcm.len() as u32)
                .map_err(|e| format!("could not allocate an audio buffer: {e}"))?;

            let mut destination: *mut u8 = std::ptr::null_mut();
            buffer
                .Lock(&mut destination, None, None)
                .map_err(|e| e.to_string())?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr(), destination, pcm.len());
            buffer.Unlock().map_err(|e| e.to_string())?;
            buffer
                .SetCurrentLength(pcm.len() as u32)
                .map_err(|e| e.to_string())?;

            let sample = MFCreateSample().map_err(|e| e.to_string())?;
            sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;

            let rate = spec.sample_rate.max(1) as i64;
            let start = self.audio_frames as i64 * UNITS_PER_SECOND / rate;
            let duration = frames as i64 * UNITS_PER_SECOND / rate;
            sample.SetSampleTime(start).map_err(|e| e.to_string())?;
            sample
                .SetSampleDuration(duration)
                .map_err(|e| e.to_string())?;

            self.writer
                .WriteSample(stream, &sample)
                .map_err(|e| format!("could not write audio: {e}"))?;
        }

        self.audio_frames += frames as u64;
        Ok(())
    }

    /// How much audio has been written, in whole samples per channel.
    pub fn audio_frames_written(&self) -> u64 {
        self.audio_frames
    }

    /// Flush and close the file. Consumes the encoder so it cannot be used after.
    pub fn finish(self) -> Result<u64, String> {
        unsafe {
            self.writer
                .Finalize()
                .map_err(|e| format!("could not finalise the recording: {e}"))?;
        }
        Ok(self.frames)
    }
}

/// Configure an AAC stream on the sink writer and return its index.
fn add_audio_stream(writer: &IMFSinkWriter, spec: AudioSpec) -> Result<u32, String> {
    if !SUPPORTED_AUDIO_RATES.contains(&spec.sample_rate) {
        return Err(format!(
            "the AAC encoder does not accept {} Hz",
            spec.sample_rate
        ));
    }
    // The encoder handles mono and stereo only. A surround device is folded down
    // to stereo upstream rather than refused.
    let channels = spec.channels.clamp(1, 2);

    unsafe {
        let out_type: IMFMediaType =
            MFCreateMediaType().map_err(|e| format!("audio output type: {e}"))?;
        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(|e| e.to_string())?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
            .map_err(|e| e.to_string())?;
        out_type
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .map_err(|e| e.to_string())?;
        out_type
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, spec.sample_rate)
            .map_err(|e| e.to_string())?;
        out_type
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .map_err(|e| e.to_string())?;
        out_type
            .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)
            .map_err(|e| e.to_string())?;

        let stream = writer
            .AddStream(&out_type)
            .map_err(|e| format!("could not add an audio stream: {e}"))?;

        let in_type: IMFMediaType =
            MFCreateMediaType().map_err(|e| format!("audio input type: {e}"))?;
        in_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(|e| e.to_string())?;
        in_type
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
            .map_err(|e| e.to_string())?;
        in_type
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .map_err(|e| e.to_string())?;
        in_type
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, spec.sample_rate)
            .map_err(|e| e.to_string())?;
        in_type
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .map_err(|e| e.to_string())?;
        in_type
            .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, channels as u32 * 2)
            .map_err(|e| e.to_string())?;

        writer
            .SetInputMediaType(stream, &in_type, None)
            .map_err(|e| format!("this machine has no AAC encoder: {e}"))?;

        Ok(stream)
    }
}

/// Pack a width and height into the single 64-bit attribute MF expects.
fn set_size(
    media_type: &IMFMediaType,
    key: windows::core::GUID,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let packed = ((width as u64) << 32) | height as u64;
    unsafe {
        media_type
            .SetUINT64(&key, packed)
            .map_err(|e| format!("setting frame size: {e}"))
    }
}

/// Same packing, for the ratio attributes.
fn set_ratio(
    media_type: &IMFMediaType,
    key: windows::core::GUID,
    numerator: u32,
    denominator: u32,
) -> Result<(), String> {
    let packed = ((numerator as u64) << 32) | denominator as u64;
    unsafe {
        media_type
            .SetUINT64(&key, packed)
            .map_err(|e| format!("setting a ratio: {e}"))
    }
}

// `encode_wide` lives on OsStrExt, which has to be in scope.
use std::os::windows::ffi::OsStrExt;

// Silences an unused-import warning when the file is checked on non-Windows,
// which never happens in practice but keeps `cargo check --all-targets` quiet.
