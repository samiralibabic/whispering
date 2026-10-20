//! Converts interleaved capture samples at the device rate into the 16 kHz
//! mono stream that every local ASR engine expects.
//!
//! Unlike the batch converter in `transcription/mod.rs`, this works
//! incrementally: input may arrive in arbitrary slice sizes (even splitting a
//! frame across calls), and output is produced as soon as enough input exists.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Input frames per resampler call. Small enough to keep latency low.
const RESAMPLER_CHUNK: usize = 1024;

pub struct MonoResampler {
    channels: usize,
    /// Interleaved samples left over from an incomplete frame.
    partial_frame: Vec<f32>,
    /// `None` when the input is already 16 kHz.
    resampler: Option<SincFixedIn<f32>>,
    /// Mono samples waiting for a full resampler chunk.
    pending: Vec<f32>,
    ratio: f64,
    mono_in: u64,
    emitted: u64,
}

impl MonoResampler {
    pub fn new(input_rate: u32, channels: u16) -> Result<Self, String> {
        if channels == 0 {
            return Err("Audio input must have at least one channel".to_string());
        }
        if input_rate < 2_000 {
            return Err(format!(
                "Sample rate {} Hz is too low (minimum 2000 Hz)",
                input_rate
            ));
        }

        let ratio = TARGET_SAMPLE_RATE as f64 / input_rate as f64;
        let resampler = if input_rate == TARGET_SAMPLE_RATE {
            None
        } else {
            // Same speech-oriented parameters as the batch converter.
            let params = SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(ratio, 8.0, params, RESAMPLER_CHUNK, 1)
                    .map_err(|e| format!("Failed to create resampler: {}", e))?,
            )
        };

        Ok(Self {
            channels: channels as usize,
            partial_frame: Vec::new(),
            resampler,
            pending: Vec::new(),
            ratio,
            mono_in: 0,
            emitted: 0,
        })
    }

    /// Converts `interleaved` and appends any ready 16 kHz samples to `out`.
    pub fn process(&mut self, interleaved: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.downmix(interleaved);

        let Some(resampler) = self.resampler.as_mut() else {
            self.emitted += self.pending.len() as u64;
            out.append(&mut self.pending);
            return Ok(());
        };

        let mut start = 0;
        while self.pending.len() - start >= RESAMPLER_CHUNK {
            let waves_in = [&self.pending[start..start + RESAMPLER_CHUNK]];
            let waves_out = resampler
                .process(&waves_in, None)
                .map_err(|e| format!("Resampling failed: {}", e))?;
            self.emitted += waves_out[0].len() as u64;
            out.extend_from_slice(&waves_out[0]);
            start += RESAMPLER_CHUNK;
        }
        self.pending.drain(..start);
        Ok(())
    }

    /// Flushes buffered input. Output length matches the input duration, the
    /// same way the batch converter truncates its zero-padded last chunk.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        // An incomplete trailing frame is discarded; it is less than one sample period.
        self.partial_frame.clear();

        let Some(resampler) = self.resampler.as_mut() else {
            self.emitted += self.pending.len() as u64;
            out.append(&mut self.pending);
            return Ok(());
        };

        let expected_total = (self.mono_in as f64 * self.ratio).round() as u64;
        let mut chunk = std::mem::take(&mut self.pending);
        // Feed zero-padded chunks until the resampler has produced the full duration.
        while self.emitted < expected_total {
            chunk.resize(RESAMPLER_CHUNK, 0.0);
            let waves_out = resampler
                .process(&[&chunk[..]], None)
                .map_err(|e| format!("Resampling failed: {}", e))?;
            let remaining = (expected_total - self.emitted) as usize;
            let take = remaining.min(waves_out[0].len());
            out.extend_from_slice(&waves_out[0][..take]);
            self.emitted += take as u64;
            chunk.clear();
        }
        Ok(())
    }

    fn downmix(&mut self, interleaved: &[f32]) {
        let channels = self.channels;
        if channels == 1 {
            self.pending.extend_from_slice(interleaved);
            self.mono_in += interleaved.len() as u64;
            return;
        }

        let mut input = interleaved;
        if !self.partial_frame.is_empty() {
            let needed = channels - self.partial_frame.len();
            let take = needed.min(input.len());
            self.partial_frame.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.partial_frame.len() < channels {
                return;
            }
            let frame = std::mem::take(&mut self.partial_frame);
            self.pending
                .push(frame.iter().sum::<f32>() / channels as f32);
            self.mono_in += 1;
        }

        let mut frames = input.chunks_exact(channels);
        for frame in &mut frames {
            self.pending
                .push(frame.iter().sum::<f32>() / channels as f32);
        }
        self.mono_in += (input.len() / channels) as u64;
        self.partial_frame.extend_from_slice(frames.remainder());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_in_slices(rate: u32, channels: u16, input: &[f32], slice: usize) -> Vec<f32> {
        let mut resampler = MonoResampler::new(rate, channels).unwrap();
        let mut out = Vec::new();
        for part in input.chunks(slice) {
            resampler.process(part, &mut out).unwrap();
        }
        resampler.finish(&mut out).unwrap();
        out
    }

    #[test]
    fn mono_16k_passes_through_unchanged() {
        let input: Vec<f32> = (0..5_000).map(|i| (i as f32 * 0.01).sin()).collect();
        assert_eq!(convert_in_slices(16_000, 1, &input, 333), input);
    }

    #[test]
    fn stereo_16k_averages_channels_even_when_frames_split_across_calls() {
        let frames = 4_001;
        let input: Vec<f32> = (0..frames * 2)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.25 })
            .collect();
        // 7 is odd, so frames are split across calls.
        let out = convert_in_slices(16_000, 2, &input, 7);
        assert_eq!(out.len(), frames);
        assert!(out.iter().all(|s| (s - 0.125).abs() < 1e-6));
    }

    #[test]
    fn resampling_48k_preserves_duration() {
        let seconds = 3;
        let input: Vec<f32> = (0..48_000 * seconds)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin() * 0.5)
            .collect();
        let out = convert_in_slices(48_000, 1, &input, 960);
        assert_eq!(out.len(), 16_000 * seconds);
        // A 440 Hz tone keeps its amplitude after resampling (away from the edges).
        let peak = out[1_000..out.len() - 1_000]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.05, "peak was {}", peak);
    }

    #[test]
    fn slice_size_does_not_change_resampled_output() {
        let input: Vec<f32> = (0..44_100 * 2)
            .map(|i| ((i as f32) * 0.003).sin() * 0.3)
            .collect();
        let a = convert_in_slices(44_100, 1, &input, 512);
        let b = convert_in_slices(44_100, 1, &input, 44_100 * 2);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
    }
}
