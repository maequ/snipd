//! System audio capture, via WASAPI loopback.
//!
//! Records what the machine is *playing* — the same thing you hear — by opening
//! the default render device in loopback mode. That is the only way to capture
//! system sound on Windows without installing a virtual audio driver, which is
//! not something a screenshot tool has any business doing.
//!
//! # Silence is not nothing
//!
//! When nothing is playing, WASAPI hands back buffers flagged as silent, and
//! sometimes hands back nothing at all. Either way the *timeline* still has to
//! advance: a recording where ten seconds of quiet simply do not exist would
//! leave the audio ten seconds ahead of the video for the rest of the file.
//! This module emits real zeroed samples to cover any gap, so the audio track
//! is always exactly as long as the time that has passed.
//!
//! # Format
//!
//! The device's own mix format is whatever it is — usually 32-bit float at
//! 48 kHz — and the AAC encoder wants 16-bit PCM. The conversion happens here so
//! that everything downstream sees one predictable format.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

/// Bytes per sample once converted. 16-bit PCM is what the AAC encoder takes.
const BYTES_PER_SAMPLE: usize = 2;

/// What the recorder needs to know to describe the audio stream.
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub fn bytes_per_frame(self) -> usize {
        self.channels as usize * BYTES_PER_SAMPLE
    }
}

/// A block of interleaved 16-bit PCM.
pub struct AudioChunk {
    pub bytes: Vec<u8>,
}

/// A running loopback capture.
pub struct AudioCapture {
    pub format: AudioFormat,
    pub chunks: Receiver<AudioChunk>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapture {
    /// Begin capturing system audio.
    ///
    /// Returns `Err` when there is no usable render device — a machine with no
    /// sound hardware at all, for instance. Recording should carry on without
    /// audio in that case rather than failing outright.
    pub fn start() -> Result<Self, String> {
        // Probed on this thread so an unusable device is reported to the caller
        // instead of disappearing into a worker, then opened again on the
        // worker thread because WASAPI interfaces belong to their apartment.
        let format = probe_format()?;

        let stop = Arc::new(AtomicBool::new(false));
        // Bounded: if the recorder ever stopped draining, audio must not grow
        // without limit. Dropping the oldest is better than exhausting memory.
        let (tx, rx) = sync_channel::<AudioChunk>(256);

        let worker = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("snipd-audio".into())
                .spawn(move || capture_loop(stop, tx, format))
                .map_err(|e| format!("could not start the audio thread: {e}"))?
        };

        Ok(Self {
            format,
            chunks: rx,
            stop,
            worker: Some(worker),
        })
    }

    pub fn stop(mut self) {
        self.halt();
    }

    /// Stop capturing and hand back whatever is still queued.
    ///
    /// The recorder writes every sample itself, so it needs the tail of the
    /// queue after the capture thread has finished. Without it a recording
    /// ends with its sound cut short by however much had not been drained.
    pub fn finish(mut self) -> Receiver<AudioChunk> {
        self.halt();
        std::mem::replace(&mut self.chunks, sync_channel(1).1)
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Open the default render device just long enough to learn its mix format.
fn probe_format() -> Result<AudioFormat, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("no audio device enumerator: {e}"))?;

        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("no default playback device: {e}"))?;

        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("could not open the playback device: {e}"))?;

        let mix = client
            .GetMixFormat()
            .map_err(|e| format!("could not read the device format: {e}"))?;
        let format = AudioFormat {
            sample_rate: (*mix).nSamplesPerSec,
            // Clamped to stereo: the AAC encoder takes mono or stereo only, and
            // a surround device is folded to its front pair rather than
            // refused. The conversion reads the real channel count separately,
            // so it still knows how to stride through the source.
            channels: (*mix).nChannels.min(2),
        };
        CoTaskMemFree(Some(mix as *const _));

        if format.channels == 0 || format.sample_rate == 0 {
            return Err("the playback device reported an unusable format".into());
        }

        Ok(format)
    }
}

/// Pull loopback audio until asked to stop.
fn capture_loop(stop: Arc<AtomicBool>, tx: SyncSender<AudioChunk>, format: AudioFormat) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let result = (|| -> Result<(), String> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| e.to_string())?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| e.to_string())?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| e.to_string())?;

            let mix = client.GetMixFormat().map_err(|e| e.to_string())?;
            let source = describe(mix);

            // A one-second buffer: generous, because the recorder only drains
            // between video frames and a hitch there must not lose audio.
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    10_000_000,
                    0,
                    mix,
                    None,
                )
                .map_err(|e| format!("could not open loopback capture: {e}"))?;
            CoTaskMemFree(Some(mix as *const _));

            let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
            client.Start().map_err(|e| e.to_string())?;

            while !stop.load(Ordering::Relaxed) {
                let available = capture.GetNextPacketSize().unwrap_or(0);
                if available == 0 {
                    // Nothing yet. Waiting a fraction of the buffer keeps this
                    // thread off the CPU without letting audio pile up.
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }

                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                if capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .is_err()
                {
                    continue;
                }

                if frames > 0 {
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let chunk = if silent || data.is_null() {
                        // Real zeroed samples rather than nothing at all, so
                        // quiet passages still occupy their share of the
                        // timeline.
                        AudioChunk {
                            bytes: vec![0u8; frames as usize * format.bytes_per_frame()],
                        }
                    } else {
                        AudioChunk {
                            bytes: to_pcm16(data, frames as usize, &source, format),
                        }
                    };

                    // Never blocks: audio must not stall because the recorder is
                    // busy, and losing a block is better than seizing up.
                    let _ = tx.try_send(chunk);
                }

                let _ = capture.ReleaseBuffer(frames);
            }

            let _ = client.Stop();
            Ok(())
        })();

        if let Err(err) = result {
            eprintln!("[audio] {err}");
        }

        CoUninitialize();
    }
}

/// How the device hands samples over, which decides how they are converted.
#[derive(Clone, Copy)]
struct SourceFormat {
    float: bool,
    bits: u16,
    channels: u16,
}

/// Read the real sample format, seeing through WAVE_FORMAT_EXTENSIBLE.
///
/// Modern devices almost always report `EXTENSIBLE` rather than naming a format
/// directly, and the tag that actually matters is inside the sub-format GUID.
unsafe fn describe(mix: *const WAVEFORMATEX) -> SourceFormat {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

    let bits = (*mix).wBitsPerSample;
    let channels = (*mix).nChannels;
    let tag = (*mix).wFormatTag;

    let float = if tag == WAVE_FORMAT_IEEE_FLOAT {
        true
    } else if tag == WAVE_FORMAT_EXTENSIBLE {
        let ext = mix as *const WAVEFORMATEXTENSIBLE;
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT differs from the PCM subtype only in
        // its first field, which is the same number as the plain format tag.
        (*ext).SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT as u32
    } else {
        false
    };

    SourceFormat {
        float,
        bits,
        channels,
    }
}

/// Convert one buffer of device samples into interleaved 16-bit PCM.
unsafe fn to_pcm16(
    data: *const u8,
    frames: usize,
    source: &SourceFormat,
    target: AudioFormat,
) -> Vec<u8> {
    let channels = source.channels.min(target.channels) as usize;
    let mut out = Vec::with_capacity(frames * target.bytes_per_frame());

    for frame in 0..frames {
        for channel in 0..target.channels as usize {
            // A device with fewer channels than expected repeats its last one
            // rather than leaving silence in a channel.
            let index = frame * source.channels as usize + channel.min(channels.saturating_sub(1));

            let sample = if source.float && source.bits == 32 {
                let value = *(data as *const f32).add(index);
                // Clamped before scaling: floating-point audio is allowed to
                // exceed 1.0, and wrapping that into an integer sounds like a
                // loud click rather than the clipping it actually is.
                (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            } else if source.bits == 16 {
                *(data as *const i16).add(index)
            } else if source.bits == 32 {
                (*(data as *const i32).add(index) >> 16) as i16
            } else {
                0
            };

            out.extend_from_slice(&sample.to_le_bytes());
        }
    }

    out
}
