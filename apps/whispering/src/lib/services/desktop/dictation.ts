import { Channel, invoke } from '@tauri-apps/api/core';
import {
	defineErrors,
	extractErrorMessage,
	type InferErrors,
} from 'wellcrafted/error';
import { tryAsync } from 'wellcrafted/result';

export const DictationError = defineErrors({
	StartFailed: ({ cause }: { cause: unknown }) => ({
		message: `Could not start transcribing while you speak: ${extractErrorMessage(cause)}`,
		cause,
	}),
	FinishFailed: ({ cause }: { cause: unknown }) => ({
		message: `Transcribing while you speak did not finish: ${extractErrorMessage(cause)}`,
		cause,
	}),
	CancelFailed: ({ cause }: { cause: unknown }) => ({
		message: `Could not cancel transcribing while you speak: ${extractErrorMessage(cause)}`,
		cause,
	}),
});
export type DictationError = InferErrors<typeof DictationError>;

/** Local engine for a session. Mirrors `EngineChoice` in `src-tauri/src/dictation/engines.rs`. */
export type DictationEngine =
	| { kind: 'whispercpp'; modelPath: string; language: string | null }
	| { kind: 'parakeet'; modelPath: string };

/** What a running session reports. Mirrors `DictationEvent`; carries no transcript text. */
export type DictationEvent =
	| {
			/** A chunk was transcribed or skipped. */
			type: 'segment';
			index: number;
			startMs: number;
			endMs: number;
			cut: 'pause' | 'forced' | 'endOfStream';
			skipped: boolean;
			inferenceMs: number;
	  }
	| {
			/** The voice is barely louder than the background. Sent at most once. */
			type: 'quietVoice';
			marginDb: number;
			atMs: number;
	  };

export type DictationMetrics = {
	audioMs: number;
	chunks: number;
	skippedChunks: number;
	forcedCuts: number;
	inferenceMsTotal: number;
	inferenceMsMax: number;
	finishMs: number;
	/** How far the voice rose above the background noise; null if too little was heard. */
	voiceMarginDb: number | null;
	/** The recording holds speech that was barely louder than the background. */
	quietVoice: boolean;
};

export type DictationResult = { text: string; metrics: DictationMetrics };

/**
 * Streaming dictation: transcribes a CPAL recording in chunks while the user speaks.
 *
 * The recording must be initialized with `streaming: true`. Call `start` after
 * the recording starts, then either `finish` (as soon as the user stops; it waits
 * for the recorder) or `cancel`.
 */
export const DictationServiceLive = {
	start: ({
		engine,
		prompt,
		onEvent,
	}: {
		engine: DictationEngine;
		prompt: string | null;
		onEvent: (event: DictationEvent) => void;
	}) => {
		const channel = new Channel<DictationEvent>();
		channel.onmessage = onEvent;
		return tryAsync({
			try: () =>
				invoke<void>('start_dictation_session', {
					engine,
					prompt,
					onProgress: channel,
				}),
			catch: (error) => DictationError.StartFailed({ cause: error }),
		});
	},

	finish: () =>
		tryAsync({
			try: () => invoke<DictationResult>('finish_dictation_session'),
			catch: (error) => DictationError.FinishFailed({ cause: error }),
		}),

	cancel: () =>
		tryAsync({
			try: () => invoke<void>('cancel_dictation_session'),
			catch: (error) => DictationError.CancelFailed({ cause: error }),
		}),
};

export type DictationService = typeof DictationServiceLive;
