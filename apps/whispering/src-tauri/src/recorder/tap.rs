//! A copy of the captured audio for streaming dictation.
//!
//! The capture callback runs on a real-time audio thread, so it must never
//! block. The tap is a lock-free single-producer, single-consumer ring buffer:
//! the callback writes into it next to the WAV writer, and the dictation worker
//! reads from it. If the reader falls behind and the buffer fills, samples are
//! dropped from the tap only (never from the WAV file) and `overflowed` is set,
//! so the session can fall back to transcribing the saved recording.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How much audio the buffer holds if the reader stalls (for example while
/// the model loads or a chunk is being transcribed).
const BUFFER_SECONDS: usize = 60;

/// Writer half, owned by the capture callback.
pub struct AudioTap {
    producer: rtrb::Producer<f32>,
    overflowed: Arc<AtomicBool>,
}

/// Reader half, handed to the dictation session.
pub struct TapReader {
    pub consumer: rtrb::Consumer<f32>,
    pub overflowed: Arc<AtomicBool>,
    /// Cleared when the recording stops, so the reader knows the stream ended.
    pub is_recording: Arc<AtomicBool>,
    pub sample_rate: u32,
    pub channels: u16,
}

pub fn audio_tap(
    sample_rate: u32,
    channels: u16,
    is_recording: Arc<AtomicBool>,
) -> (AudioTap, TapReader) {
    let capacity = sample_rate as usize * channels as usize * BUFFER_SECONDS;
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let overflowed = Arc::new(AtomicBool::new(false));
    (
        AudioTap {
            producer,
            overflowed: overflowed.clone(),
        },
        TapReader {
            consumer,
            overflowed,
            is_recording,
            sample_rate,
            channels,
        },
    )
}

impl AudioTap {
    pub fn push_f32(&mut self, samples: &[f32]) {
        self.push(samples.iter().copied(), samples.len());
    }

    /// Same conversion as `WavWriter::write_samples_i16`.
    pub fn push_i16(&mut self, samples: &[i16]) {
        self.push(
            samples.iter().map(|&s| s as f32 / i16::MAX as f32),
            samples.len(),
        );
    }

    /// Same conversion as `WavWriter::write_samples_u16`.
    pub fn push_u16(&mut self, samples: &[u16]) {
        self.push(
            samples
                .iter()
                .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0),
            samples.len(),
        );
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>, len: usize) {
        match self.producer.write_chunk_uninit(len) {
            Ok(chunk) => {
                chunk.fill_from_iter(samples);
            }
            Err(_) => self.overflowed.store(true, Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_receives_converted_samples_in_order() {
        let (mut tap, mut reader) = audio_tap(16_000, 1, Arc::new(AtomicBool::new(true)));
        tap.push_f32(&[0.25, -0.5]);
        tap.push_i16(&[i16::MAX, 0]);
        let n = reader.consumer.slots();
        let chunk = reader.consumer.read_chunk(n).unwrap();
        let (a, b) = chunk.as_slices();
        let all: Vec<f32> = a.iter().chain(b).copied().collect();
        assert_eq!(all, vec![0.25, -0.5, 1.0, 0.0]);
        assert!(!reader.overflowed.load(Ordering::Relaxed));
    }

    #[test]
    fn a_full_buffer_sets_overflowed_instead_of_blocking() {
        let (mut tap, reader) = audio_tap(2_000, 1, Arc::new(AtomicBool::new(true)));
        let second = vec![0.1f32; 2_000];
        for _ in 0..BUFFER_SECONDS {
            tap.push_f32(&second);
        }
        assert!(!reader.overflowed.load(Ordering::Relaxed));
        tap.push_f32(&second);
        assert!(reader.overflowed.load(Ordering::Relaxed));
    }
}
