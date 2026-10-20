//! Splits a growing 16 kHz mono stream into chunks that a batch ASR engine can
//! transcribe while the user keeps talking.
//!
//! Cuts prefer natural pauses. If the speaker does not pause, the chunk is cut
//! at the quietest moment shortly before `max_chunk_ms`. Every input sample
//! ends up in exactly one chunk: the chunker decides where to cut, it never
//! decides what to keep. Quiet or whispered speech is therefore never dropped;
//! a chunk is only flagged as silent when no frame rises above the noise floor.

use super::audio::TARGET_SAMPLE_RATE;

#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Analysis frame length.
    pub frame_ms: u32,
    /// A pause only ends a chunk once the chunk is at least this long.
    pub min_chunk_ms: u32,
    /// Hard upper bound for a chunk; also bounds the work left after Stop.
    pub max_chunk_ms: u32,
    /// Quiet time needed before a pause counts as a cut point.
    pub min_pause_ms: u32,
    /// Once a chunk is this long, a shorter pause (`relaxed_pause_ms`) is enough,
    /// so the chunk ends at a breath rather than being forced mid-word.
    pub relax_after_ms: u32,
    pub relaxed_pause_ms: u32,
    /// How far back from `max_chunk_ms` a forced cut looks for the quietest frame.
    pub forced_cut_search_ms: u32,
    /// A frame is quiet below `noise_floor * quiet_ratio` (3.0 is about +10 dB).
    pub quiet_ratio: f32,
    /// A chunk has activity if any frame reaches `noise_floor * activity_ratio`.
    /// Lower than `quiet_ratio` so that whispering is not mistaken for silence.
    pub activity_ratio: f32,
    /// Lower bound for the quiet threshold, so digital silence does not make
    /// every faint sound look like speech.
    pub min_quiet_rms: f32,
}

/// Tuned on Whisper large-v3-turbo, which pads every call to a 30 s window: a
/// call costs about the same for 1 s or 28 s of audio, so long chunks mean
/// fewer calls, fewer seams, and more context, while the work left after Stop
/// stays one call. A 600 ms pause avoids cutting inside hesitations and
/// self-corrections; after 22 s a 300 ms breath is enough, which in practice
/// avoids forced mid-word cuts. On real dictation, 20 s chunks halved the
/// difference from whole-file transcription compared with 12 s chunks.
impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            frame_ms: 30,
            min_chunk_ms: 20_000,
            max_chunk_ms: 28_000,
            min_pause_ms: 600,
            relax_after_ms: 22_000,
            relaxed_pause_ms: 300,
            forced_cut_search_ms: 3_000,
            quiet_ratio: 3.0,
            activity_ratio: 2.0,
            min_quiet_rms: 3e-4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CutReason {
    Pause,
    Forced,
    EndOfStream,
}

#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub index: usize,
    /// Offset of the first sample in the 16 kHz stream.
    pub start_sample: u64,
    pub samples: Vec<f32>,
    /// False when no frame rose above the noise floor (true silence).
    pub has_activity: bool,
    pub cut: CutReason,
}

impl AudioChunk {
    pub fn start_ms(&self) -> u64 {
        samples_to_ms(self.start_sample)
    }

    pub fn end_ms(&self) -> u64 {
        samples_to_ms(self.start_sample + self.samples.len() as u64)
    }
}

pub fn samples_to_ms(samples: u64) -> u64 {
    samples * 1_000 / TARGET_SAMPLE_RATE as u64
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Per-frame analysis kept for the chunk that is still being built.
#[derive(Debug, Clone, Copy)]
struct Frame {
    rms: f32,
    quiet: bool,
    active: bool,
}

/// Slowly rising, quickly falling estimate of the background level.
#[derive(Debug, Clone)]
struct NoiseFloor {
    level: Option<f32>,
}

impl NoiseFloor {
    /// Per-frame growth when the signal is above the floor (about +7% per second at 30 ms frames).
    const RISE_PER_FRAME: f32 = 1.002;
    const MIN_LEVEL: f32 = 1e-5;
    /// Starting estimate cap (about -50 dBFS). When recording starts mid-sentence
    /// the first frame is speech, not background, so it must not become the floor.
    const MAX_INITIAL_LEVEL: f32 = 3e-3;

    fn update(&mut self, rms: f32) -> f32 {
        let rms = rms.max(Self::MIN_LEVEL);
        let level = match self.level {
            None => rms.min(Self::MAX_INITIAL_LEVEL),
            Some(level) if rms < level => rms,
            Some(level) => (level * Self::RISE_PER_FRAME).min(rms),
        };
        self.level = Some(level);
        level
    }
}

pub struct Chunker {
    config: ChunkerConfig,
    frame_len: usize,
    min_chunk_frames: usize,
    max_chunk_frames: usize,
    min_pause_frames: usize,
    relax_after_frames: usize,
    relaxed_pause_frames: usize,
    search_frames: usize,
    /// Samples of the chunk being built.
    buffer: Vec<f32>,
    /// Absolute offset of `buffer[0]`.
    buffer_start: u64,
    /// One entry per complete frame in `buffer`.
    frames: Vec<Frame>,
    /// Consecutive quiet frames at the end of `frames`.
    quiet_run: usize,
    noise_floor: NoiseFloor,
    next_index: usize,
}

impl Chunker {
    pub fn new(config: ChunkerConfig) -> Self {
        let frame_len = (TARGET_SAMPLE_RATE * config.frame_ms / 1_000) as usize;
        // Minimums round up and maximums round down, so the limits hold exactly.
        let frames_at_least = |ms: u32| ms.div_ceil(config.frame_ms).max(1) as usize;
        let frames_at_most = |ms: u32| (ms / config.frame_ms).max(1) as usize;
        Self {
            frame_len,
            min_chunk_frames: frames_at_least(config.min_chunk_ms),
            max_chunk_frames: frames_at_most(config.max_chunk_ms),
            min_pause_frames: frames_at_least(config.min_pause_ms),
            relax_after_frames: frames_at_least(config.relax_after_ms),
            relaxed_pause_frames: frames_at_least(config.relaxed_pause_ms),
            search_frames: frames_at_most(config.forced_cut_search_ms),
            config,
            buffer: Vec::new(),
            buffer_start: 0,
            frames: Vec::new(),
            quiet_run: 0,
            noise_floor: NoiseFloor { level: None },
            next_index: 0,
        }
    }

    /// Adds samples and returns any chunks that became complete.
    pub fn push(&mut self, samples: &[f32]) -> Vec<AudioChunk> {
        self.buffer.extend_from_slice(samples);
        let mut ready = Vec::new();

        while self.frames.len() * self.frame_len + self.frame_len <= self.buffer.len() {
            let start = self.frames.len() * self.frame_len;
            let frame = self.classify(rms(&self.buffer[start..start + self.frame_len]));
            self.quiet_run = if frame.quiet { self.quiet_run + 1 } else { 0 };
            self.frames.push(frame);

            if let Some(chunk) = self.try_cut() {
                ready.push(chunk);
            }
        }
        ready
    }

    /// Returns whatever is left as the final chunk.
    pub fn finish(&mut self) -> Option<AudioChunk> {
        if self.buffer.is_empty() {
            return None;
        }
        let complete = self.frames.len() * self.frame_len;
        if complete < self.buffer.len() {
            let frame = self.classify(rms(&self.buffer[complete..]));
            self.frames.push(frame);
        }
        let frame_count = self.frames.len();
        let sample_count = self.buffer.len();
        Some(self.emit(frame_count, sample_count, CutReason::EndOfStream))
    }

    fn classify(&mut self, rms: f32) -> Frame {
        let floor = self.noise_floor.update(rms);
        let quiet_threshold = (floor * self.config.quiet_ratio).max(self.config.min_quiet_rms);
        let activity_threshold =
            (floor * self.config.activity_ratio).max(self.config.min_quiet_rms);
        Frame {
            rms,
            quiet: rms < quiet_threshold,
            active: rms >= activity_threshold,
        }
    }

    fn try_cut(&mut self) -> Option<AudioChunk> {
        let frame_count = self.frames.len();
        let required_pause = if frame_count >= self.relax_after_frames {
            self.relaxed_pause_frames
        } else {
            self.min_pause_frames
        };

        if frame_count >= self.min_chunk_frames && self.quiet_run >= required_pause {
            // Cut in the middle of the pause so both chunks keep a little silence,
            // but never before the minimum length (the cut stays inside the pause).
            let cut_frame = (frame_count - self.quiet_run / 2).max(self.min_chunk_frames);
            return Some(self.emit(cut_frame, cut_frame * self.frame_len, CutReason::Pause));
        }

        if frame_count >= self.max_chunk_frames {
            let search_start = frame_count.saturating_sub(self.search_frames);
            let quietest = (search_start..frame_count)
                .min_by(|&a, &b| self.frames[a].rms.total_cmp(&self.frames[b].rms))
                .unwrap_or(frame_count - 1);
            // Cut after the quietest frame.
            let cut_frame = quietest + 1;
            return Some(self.emit(cut_frame, cut_frame * self.frame_len, CutReason::Forced));
        }

        None
    }

    /// Emits the first `cut_frame` frames (`cut_sample` samples) as a chunk and
    /// keeps the rest for the next one.
    fn emit(&mut self, cut_frame: usize, cut_sample: usize, cut: CutReason) -> AudioChunk {
        let remaining_samples = self.buffer.split_off(cut_sample);
        let samples = std::mem::replace(&mut self.buffer, remaining_samples);
        let remaining_frames = self.frames.split_off(cut_frame);
        let chunk_frames = std::mem::replace(&mut self.frames, remaining_frames);

        self.quiet_run = self.frames.iter().rev().take_while(|f| f.quiet).count();

        let chunk = AudioChunk {
            index: self.next_index,
            start_sample: self.buffer_start,
            has_activity: chunk_frames.iter().any(|f| f.active),
            samples,
            cut,
        };
        self.next_index += 1;
        self.buffer_start += chunk.samples.len() as u64;
        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = TARGET_SAMPLE_RATE as usize;

    /// Short limits keep the synthetic signals small; behavior does not depend on the values.
    fn short_config() -> ChunkerConfig {
        ChunkerConfig {
            min_chunk_ms: 4_000,
            max_chunk_ms: 15_000,
            min_pause_ms: 400,
            forced_cut_search_ms: 2_000,
            ..ChunkerConfig::default()
        }
    }

    /// Deterministic pseudo-random noise so tests need no extra crates.
    struct Noise(u32);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        }
    }

    /// `segments` is a list of (seconds, amplitude). Amplitude 0 means room noise only.
    /// Speech is a tone with a 4 Hz syllable envelope that dips to 20% of the peak,
    /// because a perfectly steady tone is indistinguishable from background hum.
    fn signal(segments: &[(f32, f32)]) -> Vec<f32> {
        use std::f32::consts::PI;
        let mut noise = Noise(7);
        let mut out = Vec::new();
        for &(seconds, amplitude) in segments {
            let n = (seconds * RATE as f32) as usize;
            for i in 0..n {
                let t = i as f32 / RATE as f32;
                let tone = (t * 2.0 * PI * 220.0).sin();
                let syllables = 0.6 + 0.4 * (t * 2.0 * PI * 4.0).sin();
                out.push(tone * syllables * amplitude + noise.next() * 0.001);
            }
        }
        out
    }

    fn chunk_all(input: &[f32], slice: usize, config: ChunkerConfig) -> Vec<AudioChunk> {
        let mut chunker = Chunker::new(config);
        let mut chunks = Vec::new();
        for part in input.chunks(slice) {
            chunks.extend(chunker.push(part));
        }
        chunks.extend(chunker.finish());
        chunks
    }

    fn assert_covers(input: &[f32], chunks: &[AudioChunk]) {
        let mut offset = 0u64;
        let mut joined = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
            assert_eq!(chunk.start_sample, offset);
            offset += chunk.samples.len() as u64;
            joined.extend_from_slice(&chunk.samples);
        }
        assert_eq!(joined, input, "chunks must cover the input exactly once");
    }

    #[test]
    fn cuts_at_pauses_and_covers_every_sample() {
        // Three-second phrases separated by 0.8 s pauses, for 60 seconds.
        let mut pattern = Vec::new();
        for _ in 0..16 {
            pattern.push((3.0, 0.1));
            pattern.push((0.8, 0.0));
        }
        let input = signal(&pattern);
        let chunks = chunk_all(&input, 512, short_config());

        assert_covers(&input, &chunks);
        let (last, rest) = chunks.split_last().unwrap();
        assert_eq!(last.cut, CutReason::EndOfStream);
        for chunk in rest {
            assert_eq!(chunk.cut, CutReason::Pause);
            let ms = chunk.end_ms() - chunk.start_ms();
            assert!((4_000..=15_000).contains(&ms), "chunk length {} ms", ms);
            // Each cut lands inside a pause: the 50 ms around it is near the noise level.
            let end = chunk.samples.len();
            let edge = &chunk.samples[end - 800..];
            let peak = edge.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak < 0.01, "cut was not in a pause (peak {})", peak);
        }
    }

    #[test]
    fn continuous_speech_is_force_cut_before_max_length() {
        let input = signal(&[(40.0, 0.1)]);
        let chunks = chunk_all(&input, 1_000, short_config());

        assert_covers(&input, &chunks);
        for chunk in &chunks[..chunks.len() - 1] {
            assert_eq!(chunk.cut, CutReason::Forced);
            let ms = chunk.end_ms() - chunk.start_ms();
            assert!((13_000..=15_000).contains(&ms), "chunk length {} ms", ms);
        }
    }

    #[test]
    fn whispered_speech_counts_as_activity_and_silence_does_not() {
        // Room noise is ±0.001; whispering here is a 0.006 amplitude tone.
        let input = signal(&[(5.0, 0.0), (1.0, 0.0), (5.0, 0.006), (1.0, 0.0)]);
        let chunks = chunk_all(&input, 480, short_config());

        assert_covers(&input, &chunks);
        assert!(chunks.len() >= 2);
        assert!(!chunks[0].has_activity, "leading room noise is silence");
        assert!(
            chunks.iter().skip(1).any(|c| c.has_activity),
            "whispering must be kept for transcription"
        );
        // The whispered phrase is not split: some chunk spans all of it.
        let whisper_start = 6 * RATE as u64;
        let whisper_end = 11 * RATE as u64;
        assert!(chunks.iter().any(|c| c.start_sample <= whisper_start
            && c.start_sample + c.samples.len() as u64 >= whisper_end));
    }

    #[test]
    fn slice_size_does_not_change_cut_points() {
        let input = signal(&[(6.0, 0.1), (0.6, 0.0), (9.0, 0.08), (1.0, 0.0), (20.0, 0.1)]);
        let a = chunk_all(&input, 7, short_config());
        let b = chunk_all(&input, input.len(), short_config());
        let bounds = |chunks: &[AudioChunk]| {
            chunks
                .iter()
                .map(|c| (c.start_sample, c.samples.len(), c.cut))
                .collect::<Vec<_>>()
        };
        assert_eq!(bounds(&a), bounds(&b));
    }

    #[test]
    fn short_recording_becomes_a_single_chunk() {
        let input = signal(&[(2.5, 0.1)]);
        let chunks = chunk_all(&input, 300, short_config());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].cut, CutReason::EndOfStream);
        assert!(chunks[0].has_activity);
        assert_covers(&input, &chunks);
    }

    #[test]
    fn long_chunks_end_at_a_short_breath_instead_of_a_forced_cut() {
        // Speech with 0.35 s breaths: too short for a normal pause, long enough once relaxed.
        let mut pattern = Vec::new();
        for _ in 0..12 {
            pattern.push((3.0, 0.1));
            pattern.push((0.35, 0.0));
        }
        let input = signal(&pattern);
        let config = ChunkerConfig {
            relax_after_ms: 8_000,
            relaxed_pause_ms: 300,
            ..short_config()
        };
        let chunks = chunk_all(&input, 512, config);

        assert_covers(&input, &chunks);
        for chunk in &chunks[..chunks.len() - 1] {
            assert_eq!(chunk.cut, CutReason::Pause);
            let ms = chunk.end_ms() - chunk.start_ms();
            assert!((8_000..15_000).contains(&ms), "chunk length {} ms", ms);
        }
    }

    #[test]
    fn default_chunks_fit_in_one_whisper_window() {
        let config = ChunkerConfig::default();
        assert!(config.min_chunk_ms < config.max_chunk_ms);
        assert!(
            config.max_chunk_ms <= 30_000,
            "Whisper would split the chunk itself"
        );
    }

    #[test]
    fn empty_input_produces_no_chunks() {
        let mut chunker = Chunker::new(short_config());
        assert!(chunker.push(&[]).is_empty());
        assert!(chunker.finish().is_none());
    }
}
