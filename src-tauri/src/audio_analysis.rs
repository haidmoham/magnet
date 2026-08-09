//! Non-blocking PCM analysis for the Magnet renderer.
//!
//! [`AudioAnalysisInput::push_interleaved`] is the only method intended for an
//! audio callback. It copies complete PCM frames into a bounded, preallocated
//! queue and drops input when the analyzer is behind. FFT work, smoothing, and
//! user callbacks run exclusively on the worker thread.

use crossbeam_queue::ArrayQueue;
use librespot::playback::{
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const WINDOW_FRAMES: usize = 2_048;
const HOP_FRAMES: usize = WINDOW_FRAMES / 2;
// Dense enough to read as a spectrum rather than a handful of broad meters,
// while remaining tiny next to the 1,024-bin FFT it is derived from.
const SPECTRUM_BARS: usize = 48;
const DEFAULT_QUEUE_FRAMES: usize = 16_384;
const EMIT_INTERVAL: Duration = Duration::from_millis(33);
const SILENCE_RMS: f32 = 0.001;

#[derive(Debug, Clone, Copy)]
pub struct AnalysisConfig {
    pub sample_rate: u32,
    pub channels: usize,
    pub queue_frames: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44_100,
            channels: 2,
            queue_frames: DEFAULT_QUEUE_FRAMES,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VisualFrame {
    pub timestamp_ms: u128,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub energy: f32,
    pub peak: f32,
    pub onset: f32,
    pub spectrum: Vec<f32>,
    /// Signed left/right energy balance in the range `-1..=1`.
    pub stereo: f32,
    pub silence: bool,
}

/// The single-producer end of the analysis bus.
///
/// This type deliberately does not implement `Clone`: preserving one producer
/// lets a capacity check reserve a complete interleaved frame without locks.
pub struct AudioAnalysisInput {
    queue: Arc<ArrayQueue<f32>>,
    channels: usize,
    dropped_frames: Arc<AtomicU64>,
}

impl AudioAnalysisInput {
    /// Copies complete interleaved PCM frames without blocking or allocating.
    ///
    /// Returns the number of accepted *audio frames* (not scalar samples).
    /// An incomplete trailing frame is ignored. When capacity is exhausted,
    /// all remaining complete frames are dropped to keep the audio thread hot.
    pub fn push_interleaved(&self, samples: &[f32]) -> usize {
        self.push_converted(samples, |sample| *sample)
    }

    fn push_interleaved_f64(&self, samples: &[f64]) -> usize {
        self.push_converted(samples, |sample| *sample as f32)
    }

    fn push_converted<T, F>(&self, samples: &[T], convert: F) -> usize
    where
        F: Fn(&T) -> f32,
    {
        let complete_samples = samples.len() - samples.len() % self.channels;
        let mut accepted_frames = 0;

        for frame in samples[..complete_samples].chunks_exact(self.channels) {
            if self.queue.capacity() - self.queue.len() < self.channels {
                let dropped = (complete_samples / self.channels) - accepted_frames;
                self.dropped_frames
                    .fetch_add(dropped as u64, Ordering::Relaxed);
                break;
            }

            // With one producer, the capacity check above reserves this whole
            // frame: the consumer can only create additional free space.
            for sample in frame {
                let sample = convert(sample);
                let _ = self
                    .queue
                    .push(if sample.is_finite() { sample } else { 0.0 });
            }
            accepted_frames += 1;
        }

        accepted_frames
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }
}

/// A transparent librespot sink wrapper that taps decoded PCM before sending
/// the unchanged packet to the real output sink.
pub struct AnalysisSink {
    inner: Box<dyn Sink>,
    input: AudioAnalysisInput,
}

impl AnalysisSink {
    pub fn new(inner: Box<dyn Sink>, input: AudioAnalysisInput) -> Self {
        Self { inner, input }
    }
}

impl Sink for AnalysisSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let Ok(samples) = packet.samples() {
            self.input.push_interleaved_f64(samples);
        }
        self.inner.write(packet, converter)
    }
}

pub struct AudioAnalyzerHandle {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AudioAnalyzerHandle {
    pub fn stop(mut self) {
        self.request_stop();
        self.join();
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioAnalyzerHandle {
    fn drop(&mut self) {
        self.request_stop();
        self.join();
    }
}

/// Starts the analyzer and returns its real-time-safe input and worker handle.
///
/// The callback runs on the analyzer thread. It should hand the frame to the
/// application event system quickly rather than doing expensive work inline.
pub fn spawn_audio_analyzer<F>(
    config: AnalysisConfig,
    on_frame: F,
) -> Result<(AudioAnalysisInput, AudioAnalyzerHandle), String>
where
    F: Fn(VisualFrame) + Send + 'static,
{
    if config.sample_rate == 0 {
        return Err("analysis sample rate must be non-zero".into());
    }
    if config.channels == 0 {
        return Err("analysis channel count must be non-zero".into());
    }
    if config.queue_frames < WINDOW_FRAMES {
        return Err(format!(
            "analysis queue must hold at least {WINDOW_FRAMES} audio frames"
        ));
    }

    let queue = Arc::new(ArrayQueue::new(config.queue_frames * config.channels));
    let dropped_frames = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_queue = Arc::clone(&queue);
    let worker_stop = Arc::clone(&stop);

    let worker = thread::Builder::new()
        .name("magnet-pcm-analysis".into())
        .spawn(move || {
            analyzer_loop(config, worker_queue, worker_stop, on_frame);
        })
        .map_err(|error| format!("could not start PCM analyzer: {error}"))?;

    Ok((
        AudioAnalysisInput {
            queue,
            channels: config.channels,
            dropped_frames,
        },
        AudioAnalyzerHandle {
            stop,
            worker: Some(worker),
        },
    ))
}

fn analyzer_loop<F>(
    config: AnalysisConfig,
    queue: Arc<ArrayQueue<f32>>,
    stop: Arc<AtomicBool>,
    on_frame: F,
) where
    F: Fn(VisualFrame),
{
    let mut analyzer = Analyzer::new(config.sample_rate);
    let mut hop = vec![0.0; HOP_FRAMES * config.channels];
    let mut last_emit = Instant::now() - EMIT_INTERVAL;
    let hop_interval = Duration::from_secs_f64(HOP_FRAMES as f64 / config.sample_rate as f64);
    let mut last_analysis = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let has_audio = pop_exact(&queue, &mut hop);
        let idle_tick = !has_audio && last_analysis.elapsed() >= hop_interval;
        if has_audio || idle_tick {
            if idle_tick {
                hop.fill(0.0);
            }
            let frame = analyzer.analyze_hop(&hop, config.channels);
            last_analysis = Instant::now();
            if last_emit.elapsed() >= EMIT_INTERVAL {
                on_frame(frame);
                last_emit = Instant::now();
            }
        } else {
            // This is the worker, never the audio callback. A short sleep keeps
            // idle CPU usage negligible without adding audio-path contention.
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn pop_exact(queue: &ArrayQueue<f32>, output: &mut [f32]) -> bool {
    if queue.len() < output.len() {
        return false;
    }
    for sample in output {
        // Only this worker consumes. Once the length check succeeds, the sole
        // producer cannot remove elements, so every pop is available.
        *sample = queue.pop().unwrap_or(0.0);
    }
    true
}

struct Analyzer {
    sample_rate: u32,
    window: Vec<f32>,
    mono: Vec<f32>,
    fft_input: Vec<Complex32>,
    magnitudes: Vec<f32>,
    previous_magnitudes: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    smoothed: SmoothedFeatures,
}

struct SmoothedFeatures {
    bass: f32,
    mid: f32,
    treble: f32,
    energy: f32,
    peak: f32,
    onset: f32,
    stereo: f32,
    spectrum: [f32; SPECTRUM_BARS],
}

impl Default for SmoothedFeatures {
    fn default() -> Self {
        Self {
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            energy: 0.0,
            peak: 0.0,
            onset: 0.0,
            stereo: 0.0,
            spectrum: [0.0; SPECTRUM_BARS],
        }
    }
}

impl Analyzer {
    fn new(sample_rate: u32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(WINDOW_FRAMES);
        let window = (0..WINDOW_FRAMES)
            .map(|index| {
                0.5 - 0.5
                    * (std::f32::consts::TAU * index as f32 / (WINDOW_FRAMES - 1) as f32).cos()
            })
            .collect();

        Self {
            sample_rate,
            window,
            mono: vec![0.0; WINDOW_FRAMES],
            fft_input: vec![Complex32::default(); WINDOW_FRAMES],
            magnitudes: vec![0.0; WINDOW_FRAMES / 2],
            previous_magnitudes: vec![0.0; WINDOW_FRAMES / 2],
            fft,
            smoothed: SmoothedFeatures::default(),
        }
    }

    fn analyze_hop(&mut self, interleaved: &[f32], channels: usize) -> VisualFrame {
        self.mono.copy_within(HOP_FRAMES.., 0);

        let mut sum_squares = 0.0;
        let mut peak = 0.0_f32;
        let mut left_squares = 0.0;
        let mut right_squares = 0.0;

        for (offset, frame) in interleaved.chunks_exact(channels).enumerate() {
            let sample = |index: usize| {
                let value = frame[index];
                if value.is_finite() {
                    value
                } else {
                    0.0
                }
            };
            let mono = (0..channels).map(sample).sum::<f32>() / channels as f32;
            self.mono[HOP_FRAMES + offset] = mono;
            sum_squares += (0..channels)
                .map(|index| {
                    let sample = sample(index);
                    sample * sample
                })
                .sum::<f32>()
                / channels as f32;
            peak = peak.max(
                (0..channels)
                    .map(|index| sample(index).abs())
                    .fold(0.0, f32::max),
            );
            let left = sample(0);
            left_squares += left * left;
            let right = if channels > 1 { sample(1) } else { left };
            right_squares += right * right;
        }

        let hop_frames = interleaved.len() / channels;
        let rms = (sum_squares / hop_frames.max(1) as f32).sqrt();
        let left_rms = (left_squares / hop_frames.max(1) as f32).sqrt();
        let right_rms = (right_squares / hop_frames.max(1) as f32).sqrt();
        let stereo = (right_rms - left_rms) / (right_rms + left_rms + f32::EPSILON);

        for ((fft_sample, &sample), &window) in
            self.fft_input.iter_mut().zip(&self.mono).zip(&self.window)
        {
            *fft_sample = Complex32::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.fft_input);

        let normalization = 2.0 / WINDOW_FRAMES as f32;
        for (magnitude, bin) in self.magnitudes.iter_mut().zip(&self.fft_input) {
            *magnitude = bin.norm() * normalization;
        }

        let bass = self.band_level(20.0, 250.0);
        let mid = self.band_level(250.0, 4_000.0);
        let treble = self.band_level(4_000.0, 16_000.0);
        let positive_flux = self
            .magnitudes
            .iter()
            .zip(&self.previous_magnitudes)
            .map(|(current, previous)| (current - previous).max(0.0))
            .sum::<f32>()
            / self.magnitudes.len() as f32;
        self.previous_magnitudes.copy_from_slice(&self.magnitudes);

        // Log-like mappings make normalized music PCM expressive without
        // allowing isolated peaks to blow out the renderer.
        let bass = response(bass, 30.0);
        let mid = response(mid, 42.0);
        let treble = response(treble, 64.0);
        let energy = response(rms, 3.2);
        let peak = peak.clamp(0.0, 1.0);
        let onset = response(positive_flux, 95.0);

        self.smoothed.bass = smooth(self.smoothed.bass, bass, 0.46, 0.16);
        self.smoothed.mid = smooth(self.smoothed.mid, mid, 0.42, 0.15);
        self.smoothed.treble = smooth(self.smoothed.treble, treble, 0.50, 0.19);
        self.smoothed.energy = smooth(self.smoothed.energy, energy, 0.48, 0.18);
        self.smoothed.peak = smooth(self.smoothed.peak, peak, 0.68, 0.23);
        self.smoothed.onset = smooth(self.smoothed.onset, onset, 0.82, 0.30);
        self.smoothed.stereo = smooth(self.smoothed.stereo, stereo, 0.32, 0.18);
        for index in 0..SPECTRUM_BARS {
            let low = 20.0 * (800.0_f32).powf(index as f32 / SPECTRUM_BARS as f32);
            let high = 20.0 * (800.0_f32).powf((index + 1) as f32 / SPECTRUM_BARS as f32);
            // Emphasize the useful lower end of decoded Spotify PCM so quiet
            // passages remain legible, then let transients attack quickly and
            // fall away fast.  This keeps the histogram kinetic without
            // inventing motion when the audio is silent.
            let level = response(self.band_level(low, high), 64.0).powf(0.78);
            self.smoothed.spectrum[index] =
                smooth(self.smoothed.spectrum[index], level, 0.82, 0.10);
        }

        VisualFrame {
            timestamp_ms: timestamp_ms(),
            bass: self.smoothed.bass,
            mid: self.smoothed.mid,
            treble: self.smoothed.treble,
            energy: self.smoothed.energy,
            peak: self.smoothed.peak,
            onset: self.smoothed.onset,
            spectrum: self.smoothed.spectrum.to_vec(),
            stereo: self.smoothed.stereo.clamp(-1.0, 1.0),
            silence: rms < SILENCE_RMS,
        }
    }

    fn band_level(&self, low_hz: f32, high_hz: f32) -> f32 {
        let nyquist = self.sample_rate as f32 / 2.0;
        let high_hz = high_hz.min(nyquist);
        let low_bin = ((low_hz * WINDOW_FRAMES as f32 / self.sample_rate as f32) as usize)
            .min(self.magnitudes.len());
        let high_bin = ((high_hz * WINDOW_FRAMES as f32 / self.sample_rate as f32).ceil() as usize)
            .min(self.magnitudes.len());
        if high_bin <= low_bin {
            return 0.0;
        }
        let band = &self.magnitudes[low_bin..high_bin];
        (band.iter().map(|value| value * value).sum::<f32>() / band.len() as f32).sqrt()
    }
}

fn response(value: f32, gain: f32) -> f32 {
    (1.0 - (-value * gain).exp()).clamp(0.0, 1.0)
}

fn smooth(previous: f32, next: f32, attack: f32, release: f32) -> f32 {
    let coefficient = if next >= previous { attack } else { release };
    previous + (next - previous) * coefficient
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f32, amplitude: f32, left: f32, right: f32) -> Vec<f32> {
        (0..HOP_FRAMES)
            .flat_map(|index| {
                let phase = std::f32::consts::TAU * frequency * index as f32 / 44_100.0;
                let sample = phase.sin() * amplitude;
                [sample * left, sample * right]
            })
            .collect()
    }

    fn settled_frame(frequency: f32) -> VisualFrame {
        let mut analyzer = Analyzer::new(44_100);
        let samples = sine(frequency, 0.6, 1.0, 1.0);
        let mut frame = analyzer.analyze_hop(&samples, 2);
        for _ in 0..5 {
            frame = analyzer.analyze_hop(&samples, 2);
        }
        frame
    }

    #[test]
    fn frequency_bands_are_distinguishable() {
        let bass = settled_frame(90.0);
        let mid = settled_frame(1_000.0);
        let treble = settled_frame(8_000.0);

        assert!(bass.bass > bass.mid && bass.bass > bass.treble, "{bass:?}");
        assert!(mid.mid > mid.bass && mid.mid > mid.treble, "{mid:?}");
        assert!(
            treble.treble > treble.bass && treble.treble > treble.mid,
            "{treble:?}"
        );
    }

    #[test]
    fn detects_silence_and_stereo_balance() {
        let mut analyzer = Analyzer::new(44_100);
        let silence = vec![0.0; HOP_FRAMES * 2];
        assert!(analyzer.analyze_hop(&silence, 2).silence);

        let right_heavy = sine(440.0, 0.5, 0.1, 1.0);
        let frame = analyzer.analyze_hop(&right_heavy, 2);
        assert!(!frame.silence);
        assert!(frame.stereo > 0.1, "{frame:?}");
    }

    #[test]
    fn transient_produces_onset_then_decays() {
        let mut analyzer = Analyzer::new(44_100);
        let silence = vec![0.0; HOP_FRAMES * 2];
        analyzer.analyze_hop(&silence, 2);
        let transient = sine(1_500.0, 0.9, 1.0, 1.0);
        let attack = analyzer.analyze_hop(&transient, 2);
        let mut decay = attack.clone();
        for _ in 0..8 {
            decay = analyzer.analyze_hop(&silence, 2);
        }
        assert!(attack.onset > 0.05, "{attack:?}");
        assert!(decay.onset < attack.onset, "{decay:?}");
    }

    #[test]
    fn bounded_bus_drops_whole_frames_without_blocking() {
        let queue = Arc::new(ArrayQueue::new(4));
        let input = AudioAnalysisInput {
            queue: Arc::clone(&queue),
            channels: 2,
            dropped_frames: Arc::new(AtomicU64::new(0)),
        };
        let samples = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];

        assert_eq!(input.push_interleaved(&samples), 2);
        assert_eq!(queue.len(), 4);
        assert_eq!(input.dropped_frames(), 1);
    }

    #[test]
    fn rejects_invalid_worker_configuration() {
        let invalid = AnalysisConfig {
            queue_frames: WINDOW_FRAMES - 1,
            ..AnalysisConfig::default()
        };
        assert!(spawn_audio_analyzer(invalid, |_| {}).is_err());
    }
}
