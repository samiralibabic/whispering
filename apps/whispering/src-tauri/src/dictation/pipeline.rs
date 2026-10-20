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

/// Lets a caller lend an engine to a pipeline and keep using it afterwards.
impl<E: AsrEngine + ?Sized> AsrEngine for &mut E {
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
    /// Warn when the voice rises less than this above the background noise
    /// (see `Chunker::voice_margin_db`). On the user's recordings, normal speech
    /// measured 21–38 dB and whispering 11–16 dB; the garbled transcripts came
    /// from the quietest whispers, around 11–12 dB, though some of those were
    /// transcribed correctly. So this signals risk, not certain failure.
    pub quiet_margin_db: f32,
    /// When to check for a quiet voice (and again at twice this time).
    pub quiet_check_after_ms: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            chunker: ChunkerConfig::default(),
            base_prompt: None,
            context_chars: 200,
            quiet_margin_db: 14.0,
            quiet_check_after_ms: 5_000,
        }
    }
}

/// Words a quick transcription must contain before a quiet recording counts
/// as quiet speech rather than silence. Whisper turns noise into at most a
/// short phrase such as "Thank you."
const MIN_HEARD_WORDS: usize = 3;

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
    /// How far the voice rose above the background noise; `None` if too little was heard.
    pub voice_margin_db: Option<f32>,
    /// The recording contains speech that was barely louder than the background.
    pub quiet_voice: bool,
}

#[derive(Debug, Clone)]
pub struct DictationOutcome {
    pub text: String,
    pub metrics: DictationMetrics,
    pub segments: Vec<SegmentInfo>,
}

/// What a running session reports. Carries no transcript text.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DictationEvent {
    /// A chunk was transcribed or skipped.
    Segment(SegmentInfo),
    /// The speaker is barely louder than the background. Sent at most once.
    QuietVoice {
        #[serde(rename = "marginDb")]
        margin_db: f32,
        /// Audio time when the warning was raised.
        #[serde(rename = "atMs")]
        at_ms: u64,
    },
}

type EventFn = Box<dyn FnMut(&DictationEvent) + Send>;

pub struct DictationPipeline<E: AsrEngine> {
    engine: E,
    config: PipelineConfig,
    resampler: MonoResampler,
    chunker: Chunker,
    scratch: Vec<f32>,
    texts: Vec<String>,
    segments: Vec<SegmentInfo>,
    metrics: DictationMetrics,
    on_event: Option<EventFn>,
    quiet_checks: u8,
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
            on_event: None,
            quiet_checks: 0,
        })
    }

    /// Called after every chunk, transcribed or skipped, and for warnings.
    pub fn on_event(mut self, callback: impl FnMut(&DictationEvent) + Send + 'static) -> Self {
        self.on_event = Some(Box::new(callback));
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
        self.check_quiet_voice();
        Ok(())
    }

    /// Early in a recording, warn if the voice is barely above the background.
    ///
    /// A pause scores as low as quiet speech, so a low level alone is not
    /// enough: the speech so far is transcribed once, in the background, and
    /// only a few heard words count as quiet speech. Loud recordings never pay
    /// for this extra pass.
    fn check_quiet_voice(&mut self) {
        if self.metrics.quiet_voice || self.quiet_checks >= 2 {
            return;
        }
        let due = self.config.quiet_check_after_ms * (self.quiet_checks as u64 + 1);
        if self.chunker.elapsed_ms() < due {
            return;
        }
        self.quiet_checks += 1;
        let Some(margin) = self.chunker.voice_margin_db() else {
            return;
        };
        if margin >= self.config.quiet_margin_db {
            self.quiet_checks = 2;
            return;
        }
        let heard = self.texts.iter().any(|t| word_count(t) >= MIN_HEARD_WORDS)
            || match self.chunker.pending_speech() {
                Some(speech) => self
                    .engine
                    .transcribe(speech, None)
                    .map(|t| word_count(&t) >= MIN_HEARD_WORDS)
                    .unwrap_or(false),
                None => false,
            };
        if heard {
            self.warn_quiet_voice(margin);
        }
    }

    fn warn_quiet_voice(&mut self, margin_db: f32) {
        self.metrics.quiet_voice = true;
        if let Some(callback) = self.on_event.as_mut() {
            callback(&DictationEvent::QuietVoice {
                margin_db,
                at_ms: self.chunker.elapsed_ms(),
            });
        }
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

        let text = join_segments(&self.texts);
        self.metrics.voice_margin_db = self.chunker.voice_margin_db();
        self.metrics.quiet_voice |= self
            .metrics
            .voice_margin_db
            .is_some_and(|m| m < self.config.quiet_margin_db)
            && word_count(&text) >= MIN_HEARD_WORDS;
        self.metrics.finish_ms = started.elapsed().as_millis() as u64;
        Ok(DictationOutcome {
            text,
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
            skipped: !chunk.has_activity(),
            inference_ms: 0,
        };

        if let Some(speech) = chunk.speech.clone() {
            let prompt = self.prompt();
            let started = Instant::now();
            let text = self
                .engine
                .transcribe(&chunk.samples[speech], prompt.as_deref())
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

        if let Some(callback) = self.on_event.as_mut() {
            callback(&DictationEvent::Segment(info.clone()));
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

fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
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

    /// Short chunks so each test phrase becomes its own chunk.
    fn short_config() -> PipelineConfig {
        PipelineConfig {
            chunker: ChunkerConfig {
                min_chunk_ms: 4_000,
                max_chunk_ms: 15_000,
                min_pause_ms: 400,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        }
    }

    /// Records what it was asked to transcribe and answers "w{index}", or `reply`.
    #[derive(Clone, Default)]
    struct FakeEngine {
        calls: Arc<Mutex<Vec<(usize, Option<String>)>>>,
        fail_on_call: Option<usize>,
        reply: Option<&'static str>,
    }

    impl AsrEngine for FakeEngine {
        fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String, String> {
            let mut calls = self.calls.lock().unwrap();
            let call = calls.len();
            calls.push((samples.len(), prompt.map(str::to_string)));
            if self.fail_on_call == Some(call) {
                return Err("model crashed".to_string());
            }
            Ok(self
                .reply
                .map(str::to_string)
                .unwrap_or_else(|| format!(" w{} ", call)))
        }
    }

    /// Phrases of `phrase_s` seconds separated by 1 s pauses, at `rate` Hz stereo.
    fn stereo_phrases(rate: u32, phrases: usize, phrase_s: f32) -> Vec<f32> {
        stereo_phrases_at(rate, phrases, phrase_s, 0.1)
    }

    /// Like `stereo_phrases`, with speech of the given amplitude over room noise
    /// (about -65 dBFS). 0.0025 puts the voice about 10 dB above the noise.
    fn stereo_phrases_at(rate: u32, phrases: usize, phrase_s: f32, amplitude: f32) -> Vec<f32> {
        let noise = |i: usize| ((i * 7919) % 200) as f32 / 100_000.0 - 0.001;
        let mut out = Vec::new();
        for _ in 0..phrases {
            for i in 0..(phrase_s * rate as f32) as usize {
                let s = (i as f32 * 0.05).sin() * amplitude + noise(i);
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
            DictationPipeline::new(engine.clone(), 16_000, 2, short_config()).unwrap();
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
            short_config(),
        )
        .unwrap();
        assert_eq!(outcome.metrics.audio_ms, 3 * 6_000);
    }

    #[test]
    fn passes_base_prompt_and_preceding_text_as_context() {
        let engine = FakeEngine::default();
        let config = PipelineConfig {
            base_prompt: Some("Epicenter, Tauri".to_string()),
            ..short_config()
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
            ..short_config()
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
        let outcome = run(engine.clone(), 16_000, &input, short_config()).unwrap();

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
            short_config(),
        )
        .unwrap_err();
        assert!(error.contains("model crashed"), "{}", error);
    }

    #[test]
    fn progress_reports_every_chunk_without_text() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut pipeline = DictationPipeline::new(FakeEngine::default(), 16_000, 2, short_config())
            .unwrap()
            .on_event(move |event| {
                if let DictationEvent::Segment(info) = event {
                    sink.lock().unwrap().push(info.index);
                }
            });
        pipeline.push(&stereo_phrases(16_000, 4, 4.5)).unwrap();
        let outcome = pipeline.finish().unwrap();
        let expected: Vec<usize> = (0..outcome.segments.len()).collect();
        assert_eq!(*seen.lock().unwrap(), expected);
    }

    /// Runs `input` (16 kHz stereo) and returns the outcome, the quiet-voice
    /// warnings that were sent, and how many times the engine was called.
    fn run_watching_quiet(
        engine: FakeEngine,
        input: &[f32],
        config: PipelineConfig,
    ) -> (DictationOutcome, Vec<f32>, usize) {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let sink = warnings.clone();
        let mut pipeline = DictationPipeline::new(engine.clone(), 16_000, 2, config)
            .unwrap()
            .on_event(move |event| {
                if let DictationEvent::QuietVoice { margin_db, .. } = event {
                    sink.lock().unwrap().push(*margin_db);
                }
            });
        for part in input.chunks(640) {
            pipeline.push(part).unwrap();
        }
        let outcome = pipeline.finish().unwrap();
        let calls = engine.calls.lock().unwrap().len();
        let warnings = warnings.lock().unwrap().clone();
        (outcome, warnings, calls)
    }

    #[test]
    fn quiet_speech_is_warned_about_once_early_in_the_recording() {
        let engine = FakeEngine {
            reply: Some("this was whispered quietly"),
            ..FakeEngine::default()
        };
        let (outcome, warnings, _) = run_watching_quiet(
            engine,
            &stereo_phrases_at(16_000, 6, 4.5, 0.0025),
            PipelineConfig::default(),
        );

        let threshold = PipelineConfig::default().quiet_margin_db;
        assert_eq!(warnings.len(), 1, "exactly one warning");
        assert!(warnings[0] < threshold, "margin {}", warnings[0]);
        assert!(outcome.metrics.quiet_voice);
        assert!(outcome.metrics.voice_margin_db.unwrap() < threshold);
    }

    #[test]
    fn loud_speech_is_never_warned_about_and_costs_no_extra_transcription() {
        let (outcome, warnings, calls) = run_watching_quiet(
            FakeEngine::default(),
            &stereo_phrases(16_000, 6, 4.5),
            PipelineConfig::default(),
        );

        assert!(warnings.is_empty());
        assert!(!outcome.metrics.quiet_voice);
        assert!(outcome.metrics.voice_margin_db.unwrap() > 30.0);
        let transcribed = outcome.segments.iter().filter(|s| !s.skipped).count();
        assert_eq!(calls, transcribed, "no quick check for a loud voice");
    }

    #[test]
    fn a_quiet_recording_without_words_is_not_quiet_speech() {
        // A second of room noise, then low-level sound that Whisper only turns
        // into a hallucinated phrase.
        let engine = FakeEngine {
            reply: Some("Thank you."),
            ..FakeEngine::default()
        };
        let mut input = stereo_phrases_at(16_000, 1, 0.0, 0.0);
        input.extend(stereo_phrases_at(16_000, 2, 4.5, 0.0025));
        let (outcome, warnings, calls) =
            run_watching_quiet(engine, &input, PipelineConfig::default());

        assert!(warnings.is_empty());
        assert!(!outcome.metrics.quiet_voice);
        assert_eq!(calls, 3, "two quick checks and the final chunk");
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
