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
    IMFMediaType, IMFSinkWriter, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Video, MFStartup, MFVideoFormat_H264,
    MFVideoFormat_RGB32, MFVideoInterlace_Progressive, MFSTARTUP_NOSOCKET, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION,
};

/// One hundred-nanosecond units per second, the unit Media Foundation counts in.
const UNITS_PER_SECOND: i64 = 10_000_000;

/// Wraps a sink writer configured for one recording.
pub struct Encoder {
    writer: IMFSinkWriter,
    stream: u32,
    /// Capture dimensions, which are also the input frame dimensions.
    source_width: u32,
    source_height: u32,
    frame_duration: i64,
    frames: u64,
}

impl Encoder {
    /// Start a recording.
    ///
    /// `source_*` is the size of the frames that will be handed to [`Encoder::write_frame`];
    /// `target_*` is the size actually encoded, which is how the resolution
    /// setting takes effect.
    pub fn create(
        output: &Path,
        source_width: u32,
        source_height: u32,
        target_width: u32,
        target_height: u32,
        fps: u32,
        bitrate_bits: u32,
    ) -> Result<Self, String> {
        // H.264 requires even dimensions; an odd one is rejected outright with an
        // unhelpful error, so they are rounded down here instead.
        let target_width = target_width & !1;
        let target_height = target_height & !1;
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
            set_size(&in_type, MF_MT_FRAME_SIZE, source_width, source_height)?;
            set_ratio(&in_type, MF_MT_FRAME_RATE, fps, 1)?;
            set_ratio(&in_type, MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;

            writer
                .SetInputMediaType(stream, &in_type, None)
                .map_err(|e| {
                    format!("this machine has no encoder for {target_width}x{target_height}: {e}")
                })?;

            writer
                .BeginWriting()
                .map_err(|e| format!("could not begin writing: {e}"))?;

            Ok(Self {
                writer,
                stream,
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
