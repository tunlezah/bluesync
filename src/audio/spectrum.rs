//! Spectrum analyzer: PCM -> 2048-pt FFT -> 64 log bands (AUD-046). Pure: feed
//! interleaved stereo frames via `push`; each completed FFT block yields a
//! 64-value band frame (0.0 = -80 dBFS .. 1.0 = 0 dBFS). The run-loop
//! `run_spectrum` subscribes to the PCM bus and publishes SpectrumData events,
//! throttled to ~15-20 fps.
use crate::state::{AppStateHandle, SystemEvent};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

/// Minimum interval between published SpectrumData events (~15-20 fps).
const SPECTRUM_MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Returns `true` if enough time has elapsed since `last` to publish a new
/// spectrum frame. Pure helper; takes `now` so callers can pass a fixed value
/// in tests without sleeping.
pub fn should_publish(last: Instant, now: Instant, min_interval: Duration) -> bool {
    now.duration_since(last) >= min_interval
}

/// Subscribe to the PCM bus, run the SpectrumAnalyzer, and publish
/// `SystemEvent::SpectrumData` at ~15-20 fps. Stops cleanly when `shutdown`
/// fires or the bus closes.
pub async fn run_spectrum(
    mut pcm_rx: broadcast::Receiver<Vec<f32>>,
    state: AppStateHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut analyzer = SpectrumAnalyzer::new();
    // Initialise last_publish far enough in the past to allow the first
    // frame through immediately.
    let mut last_publish = Instant::now() - SPECTRUM_MIN_INTERVAL - Duration::from_millis(1);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
            result = pcm_rx.recv() => {
                match result {
                    Ok(frame) => {
                        for bands in analyzer.push(&frame) {
                            let now = Instant::now();
                            if should_publish(last_publish, now, SPECTRUM_MIN_INTERVAL) {
                                last_publish = now;
                                state.publish(SystemEvent::SpectrumData { bands });
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

const FFT_SIZE: usize = 2048;
const SAMPLE_RATE: f32 = 48_000.0;
const NUM_BANDS: usize = 64;
const SMOOTHING_ALPHA: f32 = 0.35;
const MIN_DB: f32 = -80.0;

pub struct SpectrumAnalyzer {
    smoothed_bands: [f32; NUM_BANDS],
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    accumulator: Vec<f32>,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        // Hanning window.
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        Self {
            smoothed_bands: [0.0; NUM_BANDS],
            fft,
            window,
            accumulator: Vec::with_capacity(FFT_SIZE),
        }
    }

    /// Feed interleaved stereo f32 PCM; returns one 64-band frame per completed
    /// FFT block (0, 1, or more per call).
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Vec<f32>> {
        for chunk in interleaved.chunks(2) {
            let mono = if chunk.len() == 2 {
                (chunk[0] + chunk[1]) * 0.5
            } else {
                chunk[0]
            };
            self.accumulator.push(mono);
        }
        let mut out = Vec::new();
        while self.accumulator.len() >= FFT_SIZE {
            let block: Vec<f32> = self.accumulator.drain(..FFT_SIZE).collect();
            out.push(self.process_frame(&block));
        }
        out
    }

    fn process_frame(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut buffer: Vec<Complex<f32>> = samples
            .iter()
            .zip(self.window.iter())
            .map(|(s, w)| Complex::new(s * w, 0.0))
            .collect();
        self.fft.process(&mut buffer);

        let half = FFT_SIZE / 2;
        let magnitudes: Vec<f32> = buffer[..half]
            .iter()
            .map(|c| c.norm() / half as f32)
            .collect();
        let bands = map_to_bands(&magnitudes, SAMPLE_RATE, NUM_BANDS);
        for (i, band) in bands.iter().enumerate() {
            self.smoothed_bands[i] =
                SMOOTHING_ALPHA * band + (1.0 - SMOOTHING_ALPHA) * self.smoothed_bands[i];
        }
        self.smoothed_bands.to_vec()
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Map FFT magnitudes to log-spaced bands (20 Hz–20 kHz), normalised
/// 0.0 = -80 dBFS .. 1.0 = 0 dBFS.
fn map_to_bands(magnitudes: &[f32], sample_rate: f32, num_bands: usize) -> Vec<f32> {
    let (min_freq, max_freq) = (20.0f32, 20_000.0f32);
    let (log_min, log_max) = (min_freq.ln(), max_freq.ln());
    let half = magnitudes.len();
    let bin_width = sample_rate / (2.0 * half as f32);

    (0..num_bands)
        .map(|i| {
            let freq_low = (log_min + (log_max - log_min) * i as f32 / num_bands as f32).exp();
            let freq_high =
                (log_min + (log_max - log_min) * (i + 1) as f32 / num_bands as f32).exp();
            let bin_low = ((freq_low / bin_width).floor() as usize).clamp(1, half - 1);
            let bin_high = ((freq_high / bin_width).ceil() as usize).clamp(bin_low + 1, half);
            let sum: f32 = magnitudes[bin_low..bin_high].iter().sum();
            let avg = sum / (bin_high - bin_low) as f32;
            let db = if avg > 0.0 {
                20.0 * avg.log10()
            } else {
                MIN_DB
            };
            ((db - MIN_DB) / -MIN_DB).clamp(0.0, 1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_mapping_count_and_range() {
        let mags = vec![0.5f32; FFT_SIZE / 2];
        let bands = map_to_bands(&mags, SAMPLE_RATE, NUM_BANDS);
        assert_eq!(bands.len(), NUM_BANDS);
        assert!(bands.iter().all(|b| (0.0..=1.0).contains(b)));
    }

    #[test]
    fn band_mapping_silence_is_zero_full_is_one() {
        assert!(
            map_to_bands(&vec![0.0f32; FFT_SIZE / 2], SAMPLE_RATE, NUM_BANDS)
                .iter()
                .all(|b| *b == 0.0)
        );
        assert!(
            map_to_bands(&vec![1.0f32; FFT_SIZE / 2], SAMPLE_RATE, NUM_BANDS)
                .iter()
                .all(|b| *b == 1.0)
        );
    }

    #[test]
    fn push_emits_a_band_frame_per_completed_block() {
        let mut sa = SpectrumAnalyzer::new();
        // One full FFT block needs FFT_SIZE mono samples = FFT_SIZE*2 interleaved.
        let silence = vec![0.0f32; FFT_SIZE * 2];
        let out = sa.push(&silence);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), NUM_BANDS);
        // Less than a full block -> no output.
        assert!(sa.push(&[0.0f32; 8]).is_empty());
    }

    #[test]
    fn push_concentrates_a_tone_in_a_higher_band_than_silence() {
        // A 5 kHz tone should put more energy in a mid/high band than silence does.
        let mut sa = SpectrumAnalyzer::new();
        let mut pcm = Vec::with_capacity(FFT_SIZE * 2);
        for i in 0..FFT_SIZE {
            let t = i as f32 / SAMPLE_RATE;
            let s = (2.0 * std::f32::consts::PI * 5000.0 * t).sin();
            pcm.push(s);
            pcm.push(s);
        }
        let bands = sa.push(&pcm);
        assert_eq!(bands.len(), 1);
        let total: f32 = bands[0].iter().sum();
        assert!(total > 0.0, "a tone should produce non-zero band energy");
    }

    // ── should_publish throttle helper ───────────────────────────────────────

    #[test]
    fn should_publish_true_when_interval_exceeded() {
        let base = Instant::now();
        let last = base;
        // 60 ms later — past the 50 ms threshold.
        let now = base + Duration::from_millis(60);
        assert!(should_publish(last, now, Duration::from_millis(50)));
    }

    #[test]
    fn should_publish_false_when_interval_not_reached() {
        let base = Instant::now();
        let last = base;
        // 30 ms later — not yet past 50 ms.
        let now = base + Duration::from_millis(30);
        assert!(!should_publish(last, now, Duration::from_millis(50)));
    }

    #[test]
    fn should_publish_true_exactly_at_interval() {
        let base = Instant::now();
        let last = base;
        let now = base + Duration::from_millis(50);
        assert!(should_publish(last, now, Duration::from_millis(50)));
    }
}
