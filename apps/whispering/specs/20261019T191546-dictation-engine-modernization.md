# Whispering: streaming dictation, correction learning, and reusable engine

Status: Proposed. Agent handoff and implementation plan; no feature changes have been made by this document.
Date: 2026-10-19
Repository: samiralibabic/whispering (main)
Target device for first iteration: M1 Max Mac Studio.

## Objective

Improve the existing Whispering fork until its everyday dictation experience approaches Wispr Flow: tap once to start and once to stop, process audio during the recording, paste polished text promptly after Stop, produce natural paragraphs, and remember corrected terminology. Preserve a path to later native/mobile/web clients without rewriting the recognition and formatting pipeline.

Local speech recognition is the default privacy boundary. Cloud *text-only* transformation through OpenAI is acceptable for now when it is chosen/configured; it is not equivalent to fully offline processing. A fast and reliable local transformer remains a provider option, not a prerequisite for shipping streaming dictation. Do not add a hosted service merely for architectural neatness.

## Instructions for the implementing agent

1. Read `AGENTS.md`, `apps/whispering/AGENTS.md`, and `apps/whispering/ARCHITECTURE.md` before editing. Follow Service -> Query -> UI separation, `Result<T, E>` service APIs, the project's Bun tooling, and established conventions.
2. Verify the current branch and code paths. The references below were inspected on `main` on 2026-10-19, but may change.
3. Begin with a measured baseline and a quick configuration fix, not a wholesale rewrite. Keep existing push-to-talk, VAD recording, batch transcription, and model providers working.
4. Implement in small, testable steps with separate commits/PRs. Feature-flag experimental streaming until baseline, regression and privacy tests pass.
5. Do not assume the user's present app settings from code defaults. Confirm selected ASR model, microphone, transformation pipeline, and output settings on the actual Mac.
6. Do not introduce remote audio processing by default, persist dictated content to logs, or send user vocabulary to hosted providers without making that disclosure explicit.
7. Keep this specification updated as discoveries change the plan; record benchmarks, rejected approaches, and new decisions in the implementation PR.

## Verified repository baseline

| Capability | What exists today | Relevant files |
| --- | --- | --- |
| Shortcut toggle | `toggleManualRecording` handles `Pressed`, while `pushToTalk` handles press/release. Default global toggle: `CommandOrControl+Shift+;` (Cmd on macOS). | `apps/whispering/src/lib/commands.ts`, `apps/whispering/src/routes/(app)/_layout-utils/register-commands.ts`, `apps/whispering/src/lib/state/device-config.svelte.ts` |
| Recording lifecycle | `startManualRecording`/`stopManualRecording` have race guards. CPAL is the default native recorder; Stop returns a completed audio blob. | `apps/whispering/src/lib/query/actions.ts`, `apps/whispering/src/lib/query/recorder.ts`, `apps/whispering/src/lib/services/desktop/recorder/cpal.ts` |
| Local ASR | Whisper, Parakeet and Moonshine adapters currently accept completed blobs and return complete strings through Tauri commands. | `apps/whispering/src/lib/query/transcription.ts`, `apps/whispering/src/lib/services/transcription/local/{whispercpp,parakeet,moonshine}.ts`, `apps/whispering/src-tauri/src/transcription/mod.rs` |
| Model caching | Rust `ModelManager` reuses an engine and has an idle timeout of five minutes. Inference currently locks a model engine. | `apps/whispering/src-tauri/src/transcription/model_manager.rs` |
| VAD | Existing voice-activated mode sends each detected utterance for processing after a pause; this is NOT an incremental transcript of a single manual recording. Quiet speech may not trigger it reliably. | `apps/whispering/src/lib/state/vad-recorder.svelte.ts`, `apps/whispering/src/lib/query/actions.ts` |
| Save/transcribe latency | Current `processRecordingPipeline` starts blob storage and transcription concurrently; an older optimization spec describes an earlier sequential bottleneck. | `apps/whispering/src/lib/query/actions.ts`, `specs/transcription-latency-optimization.md` |
| Transformations | Existing pipeline supports prompt transforms with OpenAI and other providers, plus a Custom endpoint for Ollama/LM Studio/compatible local services. | `apps/whispering/src/lib/query/transformer.ts`, `docs/guides/custom-endpoint-transformations.md` |
| Text delivery | Raw transcription is delivered first; a selected transformation may be delivered again afterward. Current behavior can paste two versions if both cursor-output settings are enabled. | `apps/whispering/src/lib/query/actions.ts`, `apps/whispering/src/lib/query/delivery.ts` |
| App architecture | Three layers: Service -> Query -> UI. Shared desktop/web logic exists; native platform functions remain adapters. | `apps/whispering/ARCHITECTURE.md`, `apps/whispering/AGENTS.md` |

These are repository facts, not measurements of the user's current installation. No end-to-end streaming manual dictation implementation was identified in the inspected paths.

## User-visible requirements

| Priority | Requirement | Acceptance criterion |
| --- | --- | --- |
| P0 | Toggle recording | One global key press starts; the next stops; no holding. Works with target app focused and survives accidental rapid presses. Existing push-to-talk remains optional. |
| P0 | One polished insertion | With a transformation selected, write exactly one formatted result to the original target field. No raw-text duplicate, no accidental Enter, and safe clipboard/focus fallback. |
| P0 | Baseline speed + privacy | Instrument stages without logging dictated content; determine where latency occurs. Audio stays local with selected local ASR; when OpenAI is used, only explicitly scoped transcript text is sent. |
| P1 | Incremental speech recognition | Capture PCM frames while recording; emit provisional and stable/final text segments. Do not wait until Stop to process a long blob. |
| P1 | Incremental formatting | Transform stable phrases/paragraphs during recording; preserve punctuation, self-corrections, technical terms and blank lines without changing meaning. Flush remaining segments on Stop. |
| P1 | Fast final response | Aim for <1 second warm-path Stop-to-paste on ordinary dictations, including multi-minute sessions. Benchmark actual p50/p95 rather than claiming this is guaranteed. |
| P1 | Dictionary and acronyms | User can add/edit/remove terms, preferred capitalization and exact/phonetic replacements; reused by both ASR and transformer. |
| P2 | Automatic learning | Detect relevant corrections to the just-inserted span where platform accessibility permits; propose/store a reversible vocabulary mapping, avoiding unrelated user edits. |
| P2 | Quiet speech / actual whispering | Benchmark whisper-level audio and background-noise profiles. Permit mic/input-gain and VAD sensitivity adjustments; never drop quiet speech because of aggressive gating. |
| P2 | Multilingual | Compare English, German and switching between them. Use model profiles if no one model gives acceptable quality. |
| Later | Cross-device clients | Keep processing and session state behind a transport-independent interface so native iOS, web and optionally a remote backend can use it. Mobile need not rely on a running Mac once local inference is viable. |

Other usability details: visible recording state, cancel/undo, recoverable recording/transcript if pasting fails, prompt output at cursor, paragraph spacing, spoken lists, domain terminology, application-aware profiles, and a reliable clipboard fallback. Avoid noisy notifications for every intermediate chunk.

## Proposed boundaries (evolve current architecture, do not replace it)

```text
Mac Tauri UI / future iPhone app / future web UI
  -> client recording control + microphone permission + text insertion
  -> DictationSession API (transport-neutral contract)
      -> Capture adapter (CPAL initially; mobile/browser later)
      -> ASR engine adapter (local streaming first; batch fallback)
      -> Segment assembler (revisions, overlap, committed boundaries)
      -> Transform adapter (OpenAI text-only | local compatible endpoint | no-op)
      -> Vocabulary service (local terms and user corrections)
      -> Session result and structured progress events
  -> client-specific cursor/clipboard delivery

Transport adapters:
  v1: in-process/Tauri commands + events (no mandatory HTTP daemon)
  later: WebSocket or other versioned transport to an isolated/local/remote engine
```

Do not put UI objects, Svelte state, Tauri IPC types, clipboard calls or global shortcuts inside the core recognition/formatting domain. Keep the existing service/query conventions and wrap native code with explicit interfaces. The client owns permission prompts, active-field insertion and OS-specific hotkeys. The engine owns audio streaming, segment stabilization, inference, transformations and vocabulary application.

Build a headless test harness around the same `DictationSession` interface before attempting a second UI. Decide whether the Rust engine eventually becomes a local daemon only after measuring a need for process isolation or remote clients; avoid a premature network dependency.

### Sketch of the session contract (illustrative, not prescribed exact TypeScript)

```ts
type DictationInput = {
  sessionId: string;
  audioFormat: { sampleRateHz: number; channels: 1; encoding: 'pcm-f32' | 'pcm-s16' };
  asr: { provider: string; model: string; language?: string; vocabularyRevision: number };
  transform: { provider: 'none' | 'openai' | 'local'; profileId?: string };
};
type EngineEvent =
  | { type: 'session.state'; sessionId: string; state: 'starting' | 'recording' | 'finalizing' | 'completed' | 'canceled' | 'failed' }
  | { type: 'transcript.partial'; sessionId: string; segmentId: string; revision: number; text: string }
  | { type: 'transcript.final'; sessionId: string; segmentId: string; revision: number; text: string; startMs: number; endMs: number }
  | { type: 'transform.final'; sessionId: string; segmentIds: string[]; revision: number; text: string }
  | { type: 'session.complete'; sessionId: string; finalText: string; metrics: Record<string, number> }
  | { type: 'session.error'; sessionId: string; stage: string; code: string };
interface DictationSession {
  start(input: DictationInput): Promise<void>;
  pushAudio(frames: Float32Array, sequence: number): Promise<void>;
  stop(): Promise<void>;
  cancel(): Promise<void>;
  subscribe(onEvent: (event: EngineEvent) => void): () => void;
}
```

Keep message versions, monotonic audio sequence numbers, stable segment IDs and revisions. Partial hypotheses must be replaceable. Only stable/final ASR segments should be submitted for text cleanup and eventual pasting. Handle overlap without dropped or duplicate words. Define backpressure limits and explicit end-of-stream flush; cancellation must stop capture and discard unfinished delivery.

### Transformation and output semantics

- Keep both OpenAI and local compatible endpoints behind the same text-transform interface. OpenAI can remain the initial quality baseline; compare with small local models on latency and meaning preservation before changing defaults.
- Prompt transformations may include a bounded neighboring context window, output style, language hint, and user vocabulary. Use stable text chunks, not the whole growing transcript on each request.
- Preserve meaning, technical names, paragraphs, capitalization, punctuation and deliberate wording. Resolve spoken revisions and filler words. Prevent hallucinated expansions of acronyms or invented content.
- Accumulate formatted segment results privately until Stop. On Stop, finalize the remaining ASR tail and any pending transforms, then assemble one final string and insert once. Do not stream provisional text into third-party text fields by default: replacing it can clobber the user's edits.
- Keep a raw-ASR-only low-latency mode; if the transformer times out/fails, offer the reliable raw transcript and an optional retry rather than losing the recording.
- Separate transformation transport timeout from recognition timing, and run independent formatting work concurrently with bounded capacity. Preserve segment order at assembly.
- Ensure only one output path inserts text. Audit `output.transcription.cursor`, `output.transformation.cursor`, clipboard options and auto-Enter settings together.

### Recognition approach

1. Benchmark existing Parakeet and Whisper backends on this M1 Max before replacing them. The current Rust APIs are completed-audio `transcribe_samples` calls, not streaming engines.
2. Investigate native chunk streaming using the CPAL PCM callback with timestamp/sequence information; do not repeatedly copy/serialize the entire accumulated blob through Tauri IPC.
3. Evaluate actual streaming support in recent Moonshine and other Rust/Core ML backends. If a candidate does not expose stable incremental hypotheses, a windowed batch ASR prototype is acceptable as a comparison, but do not mistake it for a fully streaming engine.
4. Prefetch/preload the selected model and keep it warm while the app is ready; measure idle-unload effects. Current `ModelManager` uses one engine and a five-minute idle timeout, which may constrain simultaneous ASR jobs.
5. Keep recording and ASR thread scheduling separate so inference never blocks the audio callback. Bound queues; decide how to recover after backlog, model errors or very long speech.

### Vocabulary and learning

Define a `VocabularyEntry` independent of provider: id, canonical spelling, aliases/pronunciations, case-sensitive replacement flag, language, scope/profile, source (`manual` / `suggested` / `approved-edit`), confidence, timestamps, and disabled state.

Phase A: manual dictionary + exact replacements + prompt terms. Phase B: pass suitable terms into ASR decoding where the engine supports hints, and include approved terms in transformer context. Phase C: optional learning by inspecting only an edit to a recently inserted range, where macOS Accessibility makes this possible. Distinguish a true transcription correction from semantic rewriting; never silently learn every word change. Support viewing, reversing and disabling learned entries. No retraining is required for initial vocabulary learning.

Personal terms must remain local by default. Future sync should use an explicitly designed client-encrypted format. Do not assume the existing workspace synchronization automatically makes every user-defined field end-to-end encrypted.

### Quiet/whispered speech

Test quiet normal speech separately from true whispered speech and from noise. Inspect input level, selected mic, speech/no-speech thresholds, VAD endpoint detection, audio preprocessing, silence trimming, noise suppression and model-specific confidence. Existing Whisper inference sets `no_speech_thold = 0.2`; this may warrant experiments, not an arbitrary global change. Use manual toggle recording as the safe fallback where VAD would discard quiet content. Record objective error rates on the user's own test clips.

## Privacy and provider selection

| Config | Audio leaves Mac? | Dictated text leaves Mac? |
| --- | --- | --- |
| Local ASR + no transform | No | No |
| Local ASR + local model via localhost | No (if endpoint truly local) | No |
| Local ASR + OpenAI transform | No, if no other audio upload path is selected | Yes; transcript/context sent to OpenAI |
| Future hosted ASR | Yes, by explicit user choice | Usually yes |

Show an explicit provider/privacy summary in settings. Keep the privacy status consistent with the effective ASR and transformer configuration. Strip raw audio and transcript content from telemetry, error logs, remote diagnostics, and crash reports; audit the existing analytics events and avoid adding sensitive payloads. A future hosted inference service is out of scope for the initial Mac changes. Do not claim end-to-end encryption against an inference operator unless the processing architecture actually supports that property.

## Benchmark and test plan

Use a fixed, consented test corpus: 30 seconds, 1 minute, 5 minutes, 15 minutes; English, German, mixed speech; technical names/acronyms; spoken self-corrections; ordinary quiet speech; true whispering; and realistic background noise. Include a few deliberately difficult paragraph breaks.

Instrument monotonic timestamps for: key press -> recording ready, first audio frame, ASR first partial, ASR stable segment, formatting start/finish per segment, Stop press -> audio flush, final ASR completion, final formatting completion, text insertion completion. Report warm/cold p50 and p95 Stop-to-paste and full session wall time. Capture throughput, memory, CPU/GPU load, inference backlog, and any missed/doubled text. Timings must not contain user text.

Quality checks: word error rate on references, names/acronyms retained, self-corrections resolved, punctuation/paragraph agreement, formatting quality, hallucinations/meaning changes, clipboard integrity, cursor insertion compatibility. Benchmark OpenAI-transformed results against multiple local transformer candidates with the same samples. Measure both quality and speed; do not hardcode a local model as production default until it meets the comparison criteria.

Provisional performance goal: for a warmed model and ordinary-length final segment, Stop-to-paste below one second; for long dictation, finalization delay should stay roughly independent of total recording length. This is a target, not a demonstrated capability, especially when a cloud transformation is involved.

Test safety cases: rapidly double-tap toggle; Stop while starting; Cancel during ASR or transformation; microphone disconnected; offline mode; hosted model timeout; restart/crash; no active text field; focus changes during dictation; user types after Stop; 15-minute capture; and disabled clipboard permissions. No duplicate insertion or lost recordings.

## Implementation slices

### Slice 0 — Baseline and shortcut (P0)

- [ ] Confirm installed version/settings and configure `toggleManualRecording`; reproduce any mismatch with the default `Cmd+Shift+;` and fix only if needed.
- [ ] Add non-sensitive stage timing logs behind a local debug flag; benchmark current Whisper/Parakeet and current OpenAI transform settings.
- [ ] Record a short benchmark table in the PR/spec, including normal and quiet speech.
- [ ] Audit which selected settings upload audio vs. transcript; validate with an offline/network check.

Done when: one-press toggle works reliably and baseline metrics identify the dominant latency stage.

### Slice 1 — Correct final delivery (P0)

- [ ] Refactor the pipeline so a selected transformation is completed before *one* insertion; keep raw-only mode.
- [ ] Preserve focus/cursor/clipboard behavior, formatting and blank lines. Add regression tests preventing double insertion.
- [ ] Add failure/cancel/fallback behavior and recoverable transcript storage.

Done when: raw and transformed texts cannot both be inserted accidentally and no automatic Enter is emitted unless explicitly requested.

### Slice 2 — Streaming ASR (P1)

- [ ] Define typed session contract/events and a headless replay harness independent of Svelte/Tauri.
- [ ] Stream PCM frames from CPAL without blocking capture, with bounded queues and segment offsets.
- [ ] Prototype and compare at least one native streaming ASR engine against current batch Parakeet.
- [ ] Stabilize partial/revised hypotheses, deduplicate overlap, flush on Stop; retain batch fallback.

Done when: intermediate finalized speech exists during continuous manual recording, and a long session no longer requires transcribing its full duration after Stop.

### Slice 3 — Parallel incremental formatting (P1)

- [ ] Add transform-provider interface for local compatible endpoint and OpenAI text-only API.
- [ ] Process stable segments during recording; maintain paragraph boundaries and neighboring context.
- [ ] Assemble ordered formatted text once on Stop; benchmark end-to-end quality/latency.
- [ ] Keep provider selection and privacy indicator explicit; recover cleanly from timeouts.

Done when: the Stop-to-paste path normally handles only the unfinalized tail, with one insertion and no meaning loss in the test corpus.

### Slice 4 — Terminology and quiet-speech improvements (P1/P2)

- [ ] Implement editable local vocabulary and apply approved terms to formatting and ASR where supported.
- [ ] Test whisper/quiet profiles and multilingual engine choice against recorded references.
- [ ] Add optional edit-detection learning with review/undo and conservative scope.

Done when: a corrected special term is recognized in the next suitable dictation and a quiet-voice regression suite is available.

### Slice 5 — Client portability (later)

- [ ] Keep session API/DTOs and engine lifecycle independent of Tauri; add a documented transport adapter if/when a web/mobile client actually needs it.
- [ ] Prototype iPhone-native inference and keyboard integration, then optional Mac or hosted inference fallback as a separate decision.
- [ ] Design encrypted personal vocabulary sync, with explicit threat model and server visibility assumptions.

Done when: a second minimal UI can start, stream, stop, subscribe to events, and receive a final text result without importing desktop UI code.

## Decisions and unresolved points

- Chosen now: enhance the existing fork, keep ASR local on Mac, allow a separately configured OpenAI text-only transform, and keep future clients transport-independent.
- Chosen now: no mandatory localhost server; implement an engine/session boundary first, then choose IPC/network deployment based on need.
- To test: streaming ASR backend (Moonshine versus Parakeet variants), transformation model quality, partial segment cadence, and accuracy of quiet speech.
- To decide later: automatic background insertion versus explicit confirmation; whether post-insertion correction monitoring requires opt-in; vocabulary sync design; mobile inference minimum device; confidential hosted inference.
- Out of scope for this effort: commercial launch, payment, server fleet, iOS app build, or legal/licensing analysis.

## Starting instruction for a coding agent

Read this spec and the referenced code. Begin with Slice 0 and Slice 1, producing separate small PRs (or one scoped PR if changes are tightly coupled). Show measurements and tests. Do not implement the full streaming rewrite before confirming the actual installed settings and baseline. Maintain the spec with observed results and differences from the original plan.
