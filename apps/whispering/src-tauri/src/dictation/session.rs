//! Runs a `DictationPipeline` on its own thread, fed by the recorder's tap.
//!
//! Inference blocks for a second or more per chunk, so it runs here rather
//! than on the async runtime or the audio thread. Audio keeps accumulating in
//! the tap's ring buffer while a chunk is transcribed.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{info, warn};

use super::pipeline::{
    AsrEngine, DictationEvent, DictationOutcome, DictationPipeline, PipelineConfig,
};
use crate::recorder::tap::TapReader;

pub type BoxedEngine = Box<dyn AsrEngine + Send>;

/// How often the worker checks for new audio when idle.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long `finish` waits for the recorder to stop before giving up.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an abandoned session (recording closed, never finished or
/// cancelled) keeps its thread before exiting on its own.
const ABANDONED_TIMEOUT: Duration = Duration::from_secs(60);

enum Control {
    Finish(mpsc::Sender<Result<DictationOutcome, String>>),
    Cancel,
}

pub struct DictationSession {
    control: mpsc::Sender<Control>,
    worker: Option<JoinHandle<()>>,
}

impl DictationSession {
    /// Starts the worker. The model loads on the worker thread, while audio
    /// accumulates in the tap, so starting never delays the recording.
    pub fn start(
        reader: TapReader,
        load_engine: impl FnOnce() -> Result<BoxedEngine, String> + Send + 'static,
        config: PipelineConfig,
        on_event: impl FnMut(&DictationEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let (control, control_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("dictation".to_string())
            .spawn(move || run(reader, load_engine, config, on_event, control_rx))
            .map_err(|e| format!("Failed to start dictation thread: {}", e))?;
        Ok(Self {
            control,
            worker: Some(worker),
        })
    }

    /// Waits for the recording to stop, transcribes the remaining audio, and
    /// returns the transcript. Blocks; call it off the async runtime.
    pub fn finish(mut self) -> Result<DictationOutcome, String> {
        let (reply, reply_rx) = mpsc::channel();
        self.control
            .send(Control::Finish(reply))
            .map_err(|_| "Dictation session already ended".to_string())?;
        let outcome = reply_rx
            .recv()
            .map_err(|_| "Dictation session ended without a result".to_string())?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        outcome
    }

    /// Stops the worker and discards its work. Does not wait for an in-flight chunk.
    pub fn cancel(mut self) {
        let _ = self.control.send(Control::Cancel);
        // Detach: an in-flight chunk finishes on its own and is then dropped.
        self.worker.take();
    }
}

fn run(
    mut reader: TapReader,
    load_engine: impl FnOnce() -> Result<BoxedEngine, String>,
    config: PipelineConfig,
    on_event: impl FnMut(&DictationEvent) + Send + 'static,
    control: mpsc::Receiver<Control>,
) {
    let loaded = Instant::now();
    let mut pipeline = load_engine()
        .and_then(|engine| {
            DictationPipeline::new(engine, reader.sample_rate, reader.channels, config)
        })
        .map(|pipeline| pipeline.on_event(on_event));
    match &pipeline {
        Ok(_) => info!(
            "[Dictation] engine ready in {} ms",
            loaded.elapsed().as_millis()
        ),
        Err(e) => warn!("[Dictation] engine failed to start: {}", e),
    }

    let mut abandoned_since: Option<Instant> = None;
    loop {
        let drained = drain(&mut reader, &mut pipeline);

        if reader.consumer.is_abandoned() && drained == 0 {
            let since = *abandoned_since.get_or_insert_with(Instant::now);
            if since.elapsed() > ABANDONED_TIMEOUT {
                warn!("[Dictation] session was never finished; discarding it");
                return;
            }
        }

        let wait = if drained > 0 {
            Duration::ZERO
        } else {
            POLL_INTERVAL
        };
        match control.recv_timeout(wait) {
            Ok(Control::Finish(reply)) => {
                let _ = reply.send(finish(reader, pipeline));
                return;
            }
            Ok(Control::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                info!("[Dictation] session cancelled");
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Feeds everything currently in the tap to the pipeline. Returns the sample count.
/// After a failure, audio is still drained (and discarded) so the tap never overflows.
fn drain<E: AsrEngine>(
    reader: &mut TapReader,
    pipeline: &mut Result<DictationPipeline<E>, String>,
) -> usize {
    let available = reader.consumer.slots();
    if available == 0 {
        return 0;
    }
    let Ok(chunk) = reader.consumer.read_chunk(available) else {
        return 0;
    };
    if let Ok(active) = pipeline.as_mut() {
        let (first, second) = chunk.as_slices();
        if let Err(e) = active.push(first).and_then(|_| active.push(second)) {
            warn!("[Dictation] {}", e);
            *pipeline = Err(e);
        }
    }
    chunk.commit_all();

    if pipeline.is_ok() && reader.overflowed.load(Ordering::Relaxed) {
        let message = "The audio buffer overflowed because transcription fell behind".to_string();
        warn!("[Dictation] {}", message);
        *pipeline = Err(message);
    }
    available
}

fn finish<E: AsrEngine>(
    mut reader: TapReader,
    mut pipeline: Result<DictationPipeline<E>, String>,
) -> Result<DictationOutcome, String> {
    let stop_requested = Instant::now();
    // The recorder may still be stopping; keep draining until capture has ended.
    while reader.is_recording.load(Ordering::Acquire) {
        if stop_requested.elapsed() > STOP_TIMEOUT {
            return Err("The recording did not stop".to_string());
        }
        if drain(&mut reader, &mut pipeline) == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
    // A callback that started just before the stop may still be writing.
    thread::sleep(Duration::from_millis(20));
    drain(&mut reader, &mut pipeline);

    let outcome = pipeline?.finish()?;
    let m = &outcome.metrics;
    info!(
        "[Dictation] finished: audio_ms={} chunks={} skipped={} forced={} inference_ms_total={} inference_ms_max={} finish_ms={} stop_to_result_ms={}",
        m.audio_ms,
        m.chunks,
        m.skipped_chunks,
        m.forced_cuts,
        m.inference_ms_total,
        m.inference_ms_max,
        m.finish_ms,
        stop_requested.elapsed().as_millis()
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::ChunkerConfig;
    use crate::recorder::tap::{audio_tap, AudioTap};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    struct CountingEngine(usize);

    impl AsrEngine for CountingEngine {
        fn transcribe(&mut self, _: &[f32], _: Option<&str>) -> Result<String, String> {
            self.0 += 1;
            Ok(format!("w{}", self.0))
        }
    }

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

    /// One second of 16 kHz mono: a phrase for the first half, then a pause.
    fn second_of_speech(tap: &mut AudioTap) {
        let samples: Vec<f32> = (0..16_000)
            .map(|i| {
                if i < 8_000 {
                    (i as f32 * 0.05).sin() * 0.1
                } else {
                    ((i * 7919) % 200) as f32 / 100_000.0 - 0.001
                }
            })
            .collect();
        for callback in samples.chunks(320) {
            tap.push_f32(callback);
        }
    }

    fn start(
        reader: crate::recorder::tap::TapReader,
        load: impl FnOnce() -> Result<BoxedEngine, String> + Send + 'static,
    ) -> DictationSession {
        DictationSession::start(reader, load, short_config(), |_| {}).unwrap()
    }

    #[test]
    fn finish_waits_for_the_recording_to_stop_and_transcribes_everything() {
        let is_recording = Arc::new(AtomicBool::new(true));
        let (mut tap, reader) = audio_tap(16_000, 1, is_recording.clone());
        let session = start(reader, || Ok(Box::new(CountingEngine(0))));

        let capture = thread::spawn(move || {
            for _ in 0..20 {
                second_of_speech(&mut tap);
            }
            thread::sleep(Duration::from_millis(100));
            is_recording.store(false, Ordering::Release);
        });
        // Finish is requested while capture is still running, as the app does on Stop.
        let outcome = session.finish().unwrap();
        capture.join().unwrap();

        assert_eq!(outcome.metrics.audio_ms, 20_000);
        assert!(outcome.metrics.chunks >= 3);
        assert!(outcome.text.starts_with("w1 w2"), "{}", outcome.text);
    }

    #[test]
    fn engine_load_failure_is_reported_by_finish() {
        let is_recording = Arc::new(AtomicBool::new(false));
        let (_tap, reader) = audio_tap(16_000, 1, is_recording);
        let session = start(reader, || Err("model file missing".to_string()));
        let error = session.finish().unwrap_err();
        assert!(error.contains("model file missing"), "{}", error);
    }

    #[test]
    fn overflow_fails_the_session_so_the_app_can_fall_back() {
        let is_recording = Arc::new(AtomicBool::new(true));
        let (mut tap, reader) = audio_tap(16_000, 1, is_recording.clone());
        reader.overflowed.store(true, Ordering::Relaxed);
        let session = start(reader, || Ok(Box::new(CountingEngine(0))));
        second_of_speech(&mut tap);
        is_recording.store(false, Ordering::Release);
        let error = session.finish().unwrap_err();
        assert!(error.contains("overflowed"), "{}", error);
    }

    #[test]
    fn cancel_returns_without_waiting_for_the_recording() {
        let is_recording = Arc::new(AtomicBool::new(true));
        let (_tap, reader) = audio_tap(16_000, 1, is_recording);
        let session = start(reader, || Ok(Box::new(CountingEngine(0))));
        let started = Instant::now();
        session.cancel();
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
