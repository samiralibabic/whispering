//! Turns a stream of capture samples into a transcript, one chunk at a time.
//!
//! The pipeline is synchronous and has no Tauri or threading concerns, so the
//! live session, the replay tool, and the tests all run the exact same code.

use std::time::Instant;

use serde::Serialize;

use super::audio::MonoResampler;
use super::chunker::{samples_to_ms, AudioChunk, Chunker, ChunkerConfig, CutReason};

/// A batch speech recognizer that transcribes one chunk of 16 kHz mono audio.
pub trait AsrEngine {
    /// `prompt` carries context (user vocabulary and the end of the transcript
    /// so far). Engines that cannot use a prompt ignore it.
    fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String, String>;
}

impl<E: AsrEngine + ?Sized> AsrEngine for Box<E> {
    fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String, String> {
        (**self).transcribe(samples, prompt)
    }
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub chunker: ChunkerConfig,
    /// The user's transcription prompt, prepended to every chunk's prompt.
    pub base_prompt: Option<String>,
    /// How much of the preceding transcript to pass as context. 0 disables it.
    pub context_chars: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            chunker: ChunkerConfig::default(),
            base_prompt: None,
            context_chars: 200,
        }
    }
}

/// Progress for one chunk. Deliberately carries no transcript text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentInfo {
    pub index: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub cut: CutReason,
    /// True when the chunk was silent and not sent to the engine.
    pub skipped: bool,
    pub inference_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationMetrics {
    pub audio_ms: u64,
    pub chunks: usize,
    pub skipped_chunks: usize,
    pub forced_cuts: usize,
    pub inference_ms_total: u64,
    pub inference_ms_max: u64,
    /// Time spent in `finish`: the work that remains after the user stops.
    pub finish_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DictationOutcome {
    pub text: String,
    pub metrics: DictationMetrics,
    pub segments: Vec<SegmentInfo>,
}

type ProgressFn = Box<dyn FnMut(&SegmentInfo) + Send>;

pub struct DictationPipeline<E: AsrEngine> {
    engine: E,
    config: PipelineConfig,
    resampler: MonoResampler,
    chunker: Chunker,
    scratch: Vec<f32>,
    texts: Vec<String>,
    segments: Vec<SegmentInfo>,
    metrics: DictationMetrics,
    on_progress: Option<ProgressFn>,
}

impl<E: AsrEngine> DictationPipeline<E> {
    pub fn new(
        engine: E,
        input_sample_rate: u32,
        input_channels: u16,
        config: PipelineConfig,
    ) -> Result<Self, String> {
        Ok(Self {
            engine,
            resampler: MonoResampler::new(input_sample_rate, input_channels)?,
            chunker: Chunker::new(config.chunker.clone()),
            config,
            scratch: Vec::new(),
            texts: Vec::new(),
            segments: Vec::new(),
            metrics: DictationMetrics::default(),
            on_progress: None,
        })
    }

    /// Called after every chunk, transcribed or skipped.
    pub fn on_progress(mut self, callback: impl FnMut(&SegmentInfo) + Send + 'static) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// Feeds interleaved capture samples and transcribes any chunk that completes.
    pub fn push(&mut self, interleaved: &[f32]) -> Result<(), String> {
        self.scratch.clear();
        self.resampler.process(interleaved, &mut self.scratch)?;
        if self.scratch.is_empty() {
            return Ok(());
        }
        for chunk in self.chunker.push(&self.scratch) {
            self.transcribe_chunk(chunk)?;
        }
        Ok(())
    }

    /// Flushes buffered audio, transcribes the last chunk, and assembles the transcript.
    pub fn finish(mut self) -> Result<DictationOutcome, String> {
        let started = Instant::now();

        self.scratch.clear();
        self.resampler.finish(&mut self.scratch)?;
        let flushed = std::mem::take(&mut self.scratch);
        for chunk in self.chunker.push(&flushed) {
            self.transcribe_chunk(chunk)?;
        }
        if let Some(chunk) = self.chunker.finish() {
            self.transcribe_chunk(chunk)?;
        }

        self.metrics.finish_ms = started.elapsed().as_millis() as u64;
        Ok(DictationOutcome {
            text: join_segments(&self.texts),
            metrics: self.metrics,
            segments: self.segments,
        })
    }

    fn transcribe_chunk(&mut self, chunk: AudioChunk) -> Result<(), String> {
        let mut info = SegmentInfo {
            index: chunk.index,
            start_ms: chunk.start_ms(),
            end_ms: chunk.end_ms(),
            cut: chunk.cut,
            skipped: !chunk.has_activity,
            inference_ms: 0,
        };

        if chunk.has_activity {
            let prompt = self.prompt();
            let started = Instant::now();
            let text = self
                .engine
                .transcribe(&chunk.samples, prompt.as_deref())
                .map_err(|e| format!("Chunk {} failed: {}", chunk.index, e))?;
            info.inference_ms = started.elapsed().as_millis() as u64;
            self.texts.push(text);
        }

        self.metrics.audio_ms = samples_to_ms(chunk.start_sample + chunk.samples.len() as u64);
        self.metrics.chunks += 1;
        self.metrics.skipped_chunks += info.skipped as usize;
        self.metrics.forced_cuts += (info.cut == CutReason::Forced) as usize;
        self.metrics.inference_ms_total += info.inference_ms;
        self.metrics.inference_ms_max = self.metrics.inference_ms_max.max(info.inference_ms);

        if let Some(callback) = self.on_progress.as_mut() {
            callback(&info);
        }
        self.segments.push(info);
        Ok(())
    }

    fn prompt(&self) -> Option<String> {
        let base = self
            .config
            .base_prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let transcript = if self.config.context_chars > 0 {
            join_segments(&self.texts)
        } else {
            String::new()
        };
        let context = tail_words(&transcript, self.config.context_chars);
        match (base, context.is_empty()) {
            (None, true) => None,
            (Some(base), true) => Some(base.to_string()),
            (None, false) => Some(context.to_string()),
            (Some(base), false) => Some(format!("{} {}", base, context)),
        }
    }
}

/// Joins chunk transcripts with single spaces, ignoring empty chunks.
pub fn join_segments(texts: &[String]) -> String {
    texts
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The last `max_chars` characters of `text`, starting at a word boundary.
fn tail_words(text: &str, max_chars: usize) -> &str {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text;
    }
    let start = text
        .char_indices()
        .nth(char_count - max_chars)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let tail = &text[start..];
    match tail.find(char::is_whitespace) {
        Some(space) => tail[space..].trim_start(),
        None => tail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records what it was asked to transcribe and answers "w{index}".
    #[derive(Clone, Default)]
    struct FakeEngine {
        calls: Arc<Mutex<Vec<(usize, Option<String>)>>>,
        fail_on_call: Option<usize>,
    }

    impl AsrEngine for FakeEngine {
        fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String, String> {
            let mut calls = self.calls.lock().unwrap();
            let call = calls.len();
            calls.push((samples.len(), prompt.map(str::to_string)));
            if self.fail_on_call == Some(call) {
                return Err("model crashed".to_string());
            }
            Ok(format!(" w{} ", call))
        }
    }

    /// Phrases of `phrase_s` seconds separated by 1 s pauses, at `rate` Hz stereo.
    fn stereo_phrases(rate: u32, phrases: usize, phrase_s: f32) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..phrases {
            for i in 0..(phrase_s * rate as f32) as usize {
                let s = (i as f32 * 0.05).sin() * 0.1;
                out.extend_from_slice(&[s, s]);
            }
            for i in 0..rate as usize {
                let s = ((i * 7919) % 200) as f32 / 100_000.0 - 0.001;
                out.extend_from_slice(&[s, s]);
            }
        }
        out
    }

    fn run(
        engine: FakeEngine,
        rate: u32,
        input: &[f32],
        config: PipelineConfig,
    ) -> Result<DictationOutcome, String> {
        let mut pipeline = DictationPipeline::new(engine, rate, 2, config)?;
        for part in input.chunks(rate as usize / 50) {
            pipeline.push(part)?;
        }
        pipeline.finish()
    }

    #[test]
    fn transcribes_chunks_during_input_and_joins_them_in_order() {
        let engine = FakeEngine::default();
        let input = stereo_phrases(16_000, 6, 4.5);
        let mut pipeline =
            DictationPipeline::new(engine.clone(), 16_000, 2, PipelineConfig::default()).unwrap();
        for part in input.chunks(320) {
            pipeline.push(part).unwrap();
        }
        let during = engine.calls.lock().unwrap().len();
        assert!(
            during >= 4,
            "chunks must be transcribed before finish, got {}",
            during
        );

        let outcome = pipeline.finish().unwrap();
        let calls = engine.calls.lock().unwrap().len();
        let expected: Vec<String> = (0..calls).map(|i| format!("w{}", i)).collect();
        assert_eq!(outcome.text, expected.join(" "));
        assert_eq!(outcome.metrics.chunks, outcome.segments.len());
        assert_eq!(outcome.metrics.audio_ms, 6 * 5_500);
    }

    #[test]
    fn resampled_input_keeps_its_duration() {
        let outcome = run(
            FakeEngine::default(),
            48_000,
            &stereo_phrases(48_000, 3, 5.0),
            PipelineConfig::default(),
        )
        .unwrap();
        assert_eq!(outcome.metrics.audio_ms, 3 * 6_000);
    }

    #[test]
    fn passes_base_prompt_and_preceding_text_as_context() {
        let engine = FakeEngine::default();
        let config = PipelineConfig {
            base_prompt: Some("Epicenter, Tauri".to_string()),
            ..PipelineConfig::default()
        };
        run(
            engine.clone(),
            16_000,
            &stereo_phrases(16_000, 4, 4.5),
            config,
        )
        .unwrap();

        let calls = engine.calls.lock().unwrap();
        assert_eq!(calls[0].1.as_deref(), Some("Epicenter, Tauri"));
        assert_eq!(calls[1].1.as_deref(), Some("Epicenter, Tauri w0"));
        assert_eq!(calls[2].1.as_deref(), Some("Epicenter, Tauri w0 w1"));
    }

    #[test]
    fn context_can_be_disabled() {
        let engine = FakeEngine::default();
        let config = PipelineConfig {
            context_chars: 0,
            ..PipelineConfig::default()
        };
        run(
            engine.clone(),
            16_000,
            &stereo_phrases(16_000, 3, 4.5),
            config,
        )
        .unwrap();
        assert!(engine
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, p)| p.is_none()));
    }

    #[test]
    fn silent_chunks_are_skipped_but_counted() {
        let engine = FakeEngine::default();
        // 12 s of room noise, then one phrase.
        let mut input: Vec<f32> = (0..16_000 * 12)
            .flat_map(|i| {
                let s = ((i * 7919) % 200) as f32 / 100_000.0 - 0.001;
                [s, s]
            })
            .collect();
        input.extend(stereo_phrases(16_000, 1, 3.0));
        let outcome = run(engine.clone(), 16_000, &input, PipelineConfig::default()).unwrap();

        assert!(outcome.metrics.skipped_chunks >= 1);
        assert_eq!(
            engine.calls.lock().unwrap().len(),
            outcome.metrics.chunks - outcome.metrics.skipped_chunks
        );
        assert_eq!(outcome.metrics.audio_ms, 16_000);
    }

    #[test]
    fn engine_errors_stop_the_pipeline() {
        let engine = FakeEngine {
            fail_on_call: Some(1),
            ..FakeEngine::default()
        };
        let error = run(
            engine,
            16_000,
            &stereo_phrases(16_000, 4, 4.5),
            PipelineConfig::default(),
        )
        .unwrap_err();
        assert!(error.contains("model crashed"), "{}", error);
    }

    #[test]
    fn progress_reports_every_chunk_without_text() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut pipeline =
            DictationPipeline::new(FakeEngine::default(), 16_000, 2, PipelineConfig::default())
                .unwrap()
                .on_progress(move |info| sink.lock().unwrap().push(info.index));
        pipeline.push(&stereo_phrases(16_000, 4, 4.5)).unwrap();
        let outcome = pipeline.finish().unwrap();
        let expected: Vec<usize> = (0..outcome.segments.len()).collect();
        assert_eq!(*seen.lock().unwrap(), expected);
    }

    #[test]
    fn tail_words_starts_at_a_word_boundary() {
        assert_eq!(tail_words("short", 200), "short");
        assert_eq!(tail_words("alpha beta gamma", 8), "gamma");
        assert_eq!(tail_words("über straße", 7), "straße");
    }

    #[test]
    fn join_ignores_empty_chunks() {
        let texts = vec![
            " Hello.".to_string(),
            "  ".to_string(),
            "World ".to_string(),
        ];
        assert_eq!(join_segments(&texts), "Hello. World");
    }
}
