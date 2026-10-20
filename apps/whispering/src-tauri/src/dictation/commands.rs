//! Tauri commands for streaming dictation.
//!
//! Order of calls from the frontend:
//!
//! ```text
//! init_recording_session(streaming: true) ─► start_recording ─► start_dictation_session
//!   … user speaks; chunks are transcribed and reported on the progress channel …
//! finish_dictation_session  (may be called right before stop_recording; it waits for the stop)
//! cancel_dictation_session  (instead of finish, when the recording is cancelled)
//! ```

use std::sync::Mutex;

use log::info;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use super::engines::EngineChoice;
use super::pipeline::{DictationMetrics, PipelineConfig, SegmentInfo};
use super::session::DictationSession;
use crate::recorder::AppData;
use crate::transcription::ModelManager;

#[derive(Default)]
pub struct DictationState {
    session: Mutex<Option<DictationSession>>,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "name")]
pub enum DictationError {
    /// The recording was not initialized with `streaming: true`, or its tap was already taken.
    #[error("{message}")]
    NotStreaming { message: String },
    #[error("{message}")]
    NoSession { message: String },
    /// Transcription failed or fell behind; the saved recording is still complete.
    #[error("{message}")]
    SessionFailed { message: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationResult {
    pub text: String,
    pub metrics: DictationMetrics,
}

fn lock_session(
    state: &DictationState,
) -> Result<std::sync::MutexGuard<'_, Option<DictationSession>>, DictationError> {
    state
        .session
        .lock()
        .map_err(|e| DictationError::SessionFailed {
            message: format!("Dictation state is unavailable: {}", e),
        })
}

#[tauri::command]
pub async fn start_dictation_session(
    engine: EngineChoice,
    prompt: Option<String>,
    on_progress: Channel<SegmentInfo>,
    recorder: State<'_, AppData>,
    dictation: State<'_, DictationState>,
    model_manager: State<'_, ModelManager>,
) -> Result<(), DictationError> {
    let reader = recorder
        .recorder
        .lock()
        .map_err(|e| DictationError::SessionFailed {
            message: format!("Failed to lock recorder: {}", e),
        })?
        .take_tap_reader()
        .ok_or_else(|| DictationError::NotStreaming {
            message: "The current recording was not started for streaming dictation".to_string(),
        })?;

    let config = PipelineConfig {
        base_prompt: prompt,
        ..PipelineConfig::default()
    };
    let model_manager = model_manager.inner().clone();
    let session = DictationSession::start(
        reader,
        move || engine.load(model_manager),
        config,
        move |info| {
            let _ = on_progress.send(info.clone());
        },
    )
    .map_err(|message| DictationError::SessionFailed { message })?;

    if let Some(previous) = lock_session(&dictation)?.replace(session) {
        info!("[Dictation] replacing an unfinished session");
        previous.cancel();
    }
    info!("[Dictation] session started");
    Ok(())
}

#[tauri::command]
pub async fn finish_dictation_session(
    dictation: State<'_, DictationState>,
) -> Result<DictationResult, DictationError> {
    let session = lock_session(&dictation)?
        .take()
        .ok_or_else(|| DictationError::NoSession {
            message: "No dictation session is running".to_string(),
        })?;

    let outcome = tauri::async_runtime::spawn_blocking(move || session.finish())
        .await
        .map_err(|e| DictationError::SessionFailed {
            message: format!("Dictation worker panicked: {}", e),
        })?
        .map_err(|message| DictationError::SessionFailed { message })?;

    Ok(DictationResult {
        text: outcome.text,
        metrics: outcome.metrics,
    })
}

#[tauri::command]
pub async fn cancel_dictation_session(
    dictation: State<'_, DictationState>,
) -> Result<(), DictationError> {
    if let Some(session) = lock_session(&dictation)?.take() {
        session.cancel();
    }
    Ok(())
}
