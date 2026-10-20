import type { Result } from 'wellcrafted/result';
import { Ok } from 'wellcrafted/result';
import type { WhisperingError } from '$lib/result';
import { services } from '$lib/services';
import { desktopServices } from '$lib/services/desktop';
import type { DictationEngine } from '$lib/services/desktop/dictation';
import { deviceConfig } from '$lib/state/device-config.svelte';
import { settings } from '$lib/state/settings.svelte';
import { transcribeBlob } from './transcription';

/**
 * Streaming dictation for manual recordings: chunks are transcribed while the
 * user speaks, so Stop only waits for the last chunk.
 *
 * Every failure falls back to transcribing the saved recording, which the
 * streaming path never touches. The user gets a transcript either way.
 */

/**
 * The engine to stream with, or null when streaming does not apply: it is
 * opt-in, needs the CPAL recorder (the tap lives in its audio callback), and
 * a local engine with a model selected.
 */
export function streamingEngine(): DictationEngine | null {
	if (!window.__TAURI_INTERNALS__) return null;
	if (!deviceConfig.get('transcription.streaming.enabled')) return null;
	if (deviceConfig.get('recording.method') !== 'cpal') return null;

	switch (settings.get('transcription.service')) {
		case 'whispercpp': {
			const modelPath = deviceConfig.get('transcription.whispercpp.modelPath');
			if (!modelPath) return null;
			const language = settings.get('transcription.language');
			return {
				kind: 'whispercpp',
				modelPath,
				language: language === 'auto' ? null : language,
			};
		}
		case 'parakeet': {
			const modelPath = deviceConfig.get('transcription.parakeet.modelPath');
			if (!modelPath) return null;
			return { kind: 'parakeet', modelPath };
		}
		default:
			return null;
	}
}

/**
 * Starts transcribing the current recording. Progress quietly updates the
 * recording toast. Returns false when the session could not start; the
 * recording continues and is transcribed after Stop as usual.
 */
export async function startStreaming({
	engine,
	toastId,
}: {
	engine: DictationEngine;
	toastId: string;
}): Promise<boolean> {
	let transcribed = 0;
	const { error } = await desktopServices.dictation.start({
		engine,
		prompt: settings.get('transcription.prompt') || null,
		onProgress: (progress) => {
			if (progress.skipped) return;
			transcribed += 1;
			// Toast only: no OS notification or notification log entry per chunk.
			services.toast.show({
				id: toastId,
				variant: 'success',
				title: '🎙️ Whispering is recording...',
				description: `Transcribing as you speak: ${transcribed} ${transcribed === 1 ? 'part' : 'parts'} done`,
			});
		},
	});
	if (error) {
		console.warn('[Dictation] streaming did not start:', error.message);
		return false;
	}
	return true;
}

/**
 * Requests the streamed transcript. Call it as soon as the user stops: the
 * session waits for the recorder to finish, so this overlaps with saving the
 * recording. Resolve the result with `streamedTranscriptOrBatch`.
 */
export function finishStreaming() {
	return desktopServices.dictation.finish();
}

export async function cancelStreaming() {
	const { error } = await desktopServices.dictation.cancel();
	if (error) console.warn('[Dictation]', error.message);
}

/**
 * The streamed transcript, or a batch transcription of `blob` when streaming
 * failed or produced nothing (for example if every chunk looked silent).
 */
export async function streamedTranscriptOrBatch(
	finished: ReturnType<typeof finishStreaming>,
	blob: Blob,
): Promise<Result<string, WhisperingError>> {
	const { data, error } = await finished;
	if (error) {
		console.warn(
			'[Dictation] falling back to batch transcription:',
			error.message,
		);
		return transcribeBlob(blob);
	}
	if (!data.text.trim()) {
		console.warn(
			'[Dictation] streamed transcript was empty; transcribing the recording instead',
		);
		return transcribeBlob(blob);
	}
	console.info('[Dictation] streamed transcript ready', data.metrics);
	return Ok(data.text);
}
