//! Replays recorded WAV files through the streaming dictation pipeline and
//! compares the result with whole-file (batch) transcription.
//!
//! Everything runs locally. By default only timings and a word-level
//! difference rate are printed; pass `--show-text` to print transcripts.
//!
//! ```sh
//! cargo run --release --example dictation_replay -- \
//!   --engine whispercpp --model ~/…/ggml-large-v3-turbo.bin \
//!   --compare-batch recording1.wav recording2.wav
//! ```
//!
//! Options:
//!   --engine whispercpp|parakeet   (default whispercpp)
//!   --model <path>                 model file (whisper) or directory (parakeet)
//!   --language <code>              whisper language; omit for auto-detect
//!   --prompt <text>                base prompt, like the transcription prompt setting
//!   --context-chars <n>            preceding text passed as prompt (default 200, 0 = off)
//!   --min-chunk-ms / --max-chunk-ms / --min-pause-ms /
//!   --relax-after-ms / --relaxed-pause-ms / --speech-padding-ms <n>  chunker overrides
//!   --compare-batch                also transcribe each file whole and diff the results
//!   --concat                       join all files into one stream (a stand-in for long dictation)
//!   --show-diff                    print the word spans where chunked and batch differ
//!   --show-chunks                  print one line per chunk
//!   --show-text                    print the transcripts

use std::path::PathBuf;
use std::time::Instant;

use whispering_lib::dictation::audio::MonoResampler;
use whispering_lib::dictation::engines::EngineChoice;
use whispering_lib::dictation::{
    AsrEngine, ChunkerConfig, CutReason, DictationEvent, DictationPipeline, PipelineConfig,
    SegmentInfo,
};
use whispering_lib::transcription::ModelManager;

struct Args {
    engine: String,
    model: String,
    language: Option<String>,
    prompt: Option<String>,
    context_chars: usize,
    chunker: ChunkerConfig,
    compare_batch: bool,
    concat: bool,
    show_diff: bool,
    show_chunks: bool,
    show_text: bool,
    files: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        engine: "whispercpp".to_string(),
        model: String::new(),
        language: None,
        prompt: None,
        context_chars: PipelineConfig::default().context_chars,
        chunker: ChunkerConfig::default(),
        compare_batch: false,
        concat: false,
        show_diff: false,
        show_chunks: false,
        show_text: false,
        files: Vec::new(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{} needs a value", arg));
        match arg.as_str() {
            "--engine" => args.engine = value()?,
            "--model" => args.model = value()?,
            "--language" => args.language = Some(value()?),
            "--prompt" => args.prompt = Some(value()?),
            "--context-chars" => {
                args.context_chars = value()?
                    .parse()
                    .map_err(|e| format!("--context-chars: {}", e))?
            }
            "--min-chunk-ms" => args.chunker.min_chunk_ms = parse_ms(&value()?)?,
            "--max-chunk-ms" => args.chunker.max_chunk_ms = parse_ms(&value()?)?,
            "--min-pause-ms" => args.chunker.min_pause_ms = parse_ms(&value()?)?,
            "--relax-after-ms" => args.chunker.relax_after_ms = parse_ms(&value()?)?,
            "--relaxed-pause-ms" => args.chunker.relaxed_pause_ms = parse_ms(&value()?)?,
            "--speech-padding-ms" => args.chunker.speech_padding_ms = parse_ms(&value()?)?,
            "--compare-batch" => args.compare_batch = true,
            "--concat" => args.concat = true,
            "--show-diff" => args.show_diff = true,
            "--show-chunks" => args.show_chunks = true,
            "--show-text" => args.show_text = true,
            flag if flag.starts_with("--") => return Err(format!("Unknown option {}", flag)),
            file => args.files.push(PathBuf::from(file)),
        }
    }
    if args.model.is_empty() {
        return Err("--model is required".to_string());
    }
    if args.files.is_empty() {
        return Err("Pass at least one WAV file".to_string());
    }
    Ok(args)
}

fn parse_ms(value: &str) -> Result<u32, String> {
    value.parse().map_err(|e| format!("{}: {}", value, e))
}

fn engine_choice(args: &Args) -> Result<EngineChoice, String> {
    match args.engine.as_str() {
        "whispercpp" => Ok(EngineChoice::Whisper {
            model_path: args.model.clone(),
            language: args.language.clone(),
        }),
        "parakeet" => Ok(EngineChoice::Parakeet {
            model_path: args.model.clone(),
        }),
        other => Err(format!("Unknown engine {}", other)),
    }
}

/// Interleaved f32 samples plus format.
fn read_wav(path: &PathBuf) -> Result<(Vec<f32>, u32, u16), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, _) => reader.samples::<f32>().collect::<Result<_, _>>(),
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32_768.0))
            .collect::<Result<_, _>>(),
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 2_147_483_648.0))
            .collect::<Result<_, _>>(),
        (_, bits) => {
            return Err(format!(
                "{}: unsupported {}-bit integer WAV",
                path.display(),
                bits
            ))
        }
    }
    .map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok((samples, spec.sample_rate, spec.channels))
}

/// Estimates when the transcript would be ready after Stop if the file had
/// been captured in real time with the measured per-chunk inference times.
fn simulated_stop_latency_ms(segments: &[SegmentInfo], audio_ms: u64) -> u64 {
    let mut worker_free_at = 0u64;
    for segment in segments {
        // A pause cut is decided about half a pause (200 ms) after the cut point.
        let available_at = match segment.cut {
            CutReason::Pause => segment.end_ms + 200,
            CutReason::Forced => segment.end_ms,
            CutReason::EndOfStream => audio_ms,
        };
        worker_free_at = worker_free_at.max(available_at) + segment.inference_ms;
    }
    worker_free_at.saturating_sub(audio_ms)
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-level alignment: edit count plus the differing spans as (chunked, batch) text.
fn word_diff(a: &[String], b: &[String]) -> (usize, Vec<(String, String)>) {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let substitution = d[i - 1][j - 1] + (a[i - 1] != b[j - 1]) as usize;
            d[i][j] = substitution.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }

    // Walk back, grouping consecutive edits into spans.
    let (mut i, mut j) = (n, m);
    let mut spans = Vec::new();
    let (mut span_a, mut span_b): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    fn flush(span_a: &mut Vec<&str>, span_b: &mut Vec<&str>, spans: &mut Vec<(String, String)>) {
        if !span_a.is_empty() || !span_b.is_empty() {
            span_a.reverse();
            span_b.reverse();
            spans.push((span_a.join(" "), span_b.join(" ")));
            span_a.clear();
            span_b.clear();
        }
    }
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] && d[i][j] == d[i - 1][j - 1] {
            flush(&mut span_a, &mut span_b, &mut spans);
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 {
            span_a.push(&a[i - 1]);
            span_b.push(&b[j - 1]);
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            span_a.push(&a[i - 1]);
            i -= 1;
        } else {
            span_b.push(&b[j - 1]);
            j -= 1;
        }
    }
    flush(&mut span_a, &mut span_b, &mut spans);
    spans.reverse();
    (d[n][m], spans)
}

/// Joins files into one 16 kHz mono stream, as if dictated in one go.
fn read_concatenated(paths: &[PathBuf]) -> Result<(Vec<f32>, u32, u16), String> {
    let mut all = Vec::new();
    for path in paths {
        let (samples, rate, channels) = read_wav(path)?;
        let mut resampler = MonoResampler::new(rate, channels)?;
        resampler.process(&samples, &mut all)?;
        resampler.finish(&mut all)?;
    }
    Ok((all, 16_000, 1))
}

fn replay_file(
    label: &str,
    (samples, rate, channels): (Vec<f32>, u32, u16),
    args: &Args,
    engine: &mut Box<dyn AsrEngine + Send>,
) -> Result<(), String> {
    let config = PipelineConfig {
        chunker: args.chunker.clone(),
        base_prompt: args.prompt.clone(),
        context_chars: args.context_chars,
        ..PipelineConfig::default()
    };

    // Feed 20 ms slices, like the capture callback would.
    let slice = (rate as usize / 50) * channels as usize;
    let warned_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = warned_at.clone();
    let mut pipeline =
        DictationPipeline::new(&mut **engine, rate, channels, config)?.on_event(move |event| {
            if let DictationEvent::QuietVoice { at_ms, .. } = event {
                *sink.lock().unwrap() = Some(*at_ms);
            }
        });
    let started = Instant::now();
    for part in samples.chunks(slice) {
        pipeline.push(part)?;
    }
    let outcome = pipeline.finish()?;
    let wall_ms = started.elapsed().as_millis() as u64;

    let m = &outcome.metrics;
    println!(
        "\n== {} ({:.1} s, {} Hz, {} ch)",
        label,
        m.audio_ms as f64 / 1000.0,
        rate,
        channels
    );
    if args.show_chunks {
        for s in &outcome.segments {
            println!(
                "   #{:<3} {:>7.2}-{:>7.2} s  {:<11} {}",
                s.index,
                s.start_ms as f64 / 1000.0,
                s.end_ms as f64 / 1000.0,
                format!("{:?}", s.cut),
                if s.skipped {
                    "skipped (silent)".to_string()
                } else {
                    format!("{} ms", s.inference_ms)
                }
            );
        }
    }
    println!(
        "   chunks {} (skipped {}, forced {}), inference total {} ms, max {} ms, RTF {:.3}",
        m.chunks,
        m.skipped_chunks,
        m.forced_cuts,
        m.inference_ms_total,
        m.inference_ms_max,
        m.inference_ms_total as f64 / m.audio_ms.max(1) as f64
    );
    println!(
        "   finish (tail) {} ms, simulated Stop-to-transcript {} ms, replay wall {} ms",
        m.finish_ms,
        simulated_stop_latency_ms(&outcome.segments, m.audio_ms),
        wall_ms
    );
    let warned_at = *warned_at.lock().unwrap();
    println!(
        "   voice above noise {}, quiet warning: {}, quiet note after Stop: {}",
        m.voice_margin_db
            .map_or("n/a".to_string(), |db| format!("{:.1} dB", db)),
        warned_at.map_or("no".to_string(), |ms| format!(
            "at {:.1} s",
            ms as f64 / 1000.0
        )),
        if m.quiet_voice { "yes" } else { "no" }
    );

    if args.compare_batch {
        let mut mono = Vec::new();
        let mut resampler = MonoResampler::new(rate, channels)?;
        resampler.process(&samples, &mut mono)?;
        resampler.finish(&mut mono)?;
        let started = Instant::now();
        let batch = engine.transcribe(&mono, args.prompt.as_deref())?;
        let batch_ms = started.elapsed().as_millis();

        let chunked_words = normalized_words(&outcome.text);
        let batch_words = normalized_words(&batch);
        let (edits, spans) = word_diff(&chunked_words, &batch_words);
        println!(
            "   batch {} ms; words chunked {} vs batch {}, edits {} ({:.1}% of batch)",
            batch_ms,
            chunked_words.len(),
            batch_words.len(),
            edits,
            100.0 * edits as f64 / batch_words.len().max(1) as f64
        );
        if args.show_diff {
            for (chunked, batch) in &spans {
                println!("   ~ chunked [{}]  batch [{}]", chunked, batch);
            }
        }
        if args.show_text {
            println!("   --- batch\n{}", batch.trim());
        }
    }
    if args.show_text {
        println!("   --- chunked\n{}", outcome.text);
    }
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!(
                "{}\n\nSee the header of examples/dictation_replay.rs for usage.",
                e
            );
            std::process::exit(2);
        }
    };

    let load_started = Instant::now();
    let mut engine = match engine_choice(&args).and_then(|c| c.load(ModelManager::new())) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("Failed to load model: {}", e);
            std::process::exit(1);
        }
    };
    println!(
        "{} model loaded in {} ms",
        args.engine,
        load_started.elapsed().as_millis()
    );

    let inputs: Vec<(String, Result<(Vec<f32>, u32, u16), String>)> = if args.concat {
        let label = format!("{} files concatenated", args.files.len());
        vec![(label, read_concatenated(&args.files))]
    } else {
        args.files
            .iter()
            .map(|f| (f.display().to_string(), read_wav(f)))
            .collect()
    };

    let mut failures = 0;
    for (label, audio) in inputs {
        if let Err(e) = audio.and_then(|audio| replay_file(&label, audio, &args, &mut engine)) {
            eprintln!("\n== {}: {}", label, e);
            failures += 1;
        }
    }
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
