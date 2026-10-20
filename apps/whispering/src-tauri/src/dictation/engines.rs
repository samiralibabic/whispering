//! `AsrEngine` adapters over the cached engines in `ModelManager`, so chunked
//! dictation runs on the same models and inference settings as batch transcription.

use std::path::PathBuf;

use serde::Deserialize;

use super::pipeline::AsrEngine;
use crate::transcription::{transcribe_samples_parakeet, transcribe_samples_whisper, ModelManager};

/// Which local engine a dictation session uses. Mirrors the transcription service ids.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EngineChoice {
    #[serde(rename = "whispercpp", rename_all = "camelCase")]
    Whisper {
        model_path: String,
        language: Option<String>,
    },
    #[serde(rename = "parakeet", rename_all = "camelCase")]
    Parakeet { model_path: String },
}

impl EngineChoice {
    /// Builds the engine and loads its model, so the first chunk does not wait for loading.
    pub fn load(self, model_manager: ModelManager) -> Result<Box<dyn AsrEngine + Send>, String> {
        match self {
            EngineChoice::Whisper {
                model_path,
                language,
            } => {
                let model_path = PathBuf::from(model_path);
                model_manager.get_or_load_whisper(model_path.clone())?;
                Ok(Box::new(WhisperAsr {
                    model_manager,
                    model_path,
                    language,
                }))
            }
            EngineChoice::Parakeet { model_path } => {
                let model_path = PathBuf::from(model_path);
                model_manager.get_or_load_parakeet(model_path.clone())?;
                Ok(Box::new(ParakeetAsr {
                    model_manager,
                    model_path,
                }))
            }
        }
    }
}

pub struct WhisperAsr {
    model_manager: ModelManager,
    model_path: PathBuf,
    language: Option<String>,
}

impl AsrEngine for WhisperAsr {
    fn transcribe(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<String, String> {
        transcribe_samples_whisper(
            &self.model_manager,
            &self.model_path,
            samples.to_vec(),
            self.language.clone(),
            prompt.map(str::to_string),
        )
        .map_err(|e| e.to_string())
    }
}

/// Parakeet has no prompt input, so context is ignored.
pub struct ParakeetAsr {
    model_manager: ModelManager,
    model_path: PathBuf,
}

impl AsrEngine for ParakeetAsr {
    fn transcribe(&mut self, samples: &[f32], _prompt: Option<&str>) -> Result<String, String> {
        transcribe_samples_parakeet(&self.model_manager, &self.model_path, samples.to_vec())
            .map_err(|e| e.to_string())
    }
}
