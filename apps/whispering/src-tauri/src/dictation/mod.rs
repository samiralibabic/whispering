//! Streaming dictation: transcribe a manual recording while the user speaks.
//!
//! Local ASR engines only transcribe complete buffers, so the stream is cut
//! at natural pauses and each chunk is transcribed in the background. When the
//! user stops, only the last chunk is left to transcribe.
//!
//! ```text
//! capture samples ─► MonoResampler ─► Chunker ─► AsrEngine ─► join in order
//!   (device rate,      (16 kHz mono)   (pause or    (one chunk     ─► transcript
//!    interleaved)                       forced cut)  at a time)
//! ```

pub mod audio;
pub mod chunker;
pub mod pipeline;

pub use chunker::{ChunkerConfig, CutReason};
pub use pipeline::{
    AsrEngine, DictationMetrics, DictationOutcome, DictationPipeline, PipelineConfig, SegmentInfo,
};
