import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { remove } from '@tauri-apps/plugin-fs';
import { Err, Ok, type Result, tryAsync } from 'wellcrafted/result';
import type {
	CancelRecordingResult,
	WhisperingRecordingState,
} from '$lib/constants/audio';
import { FsServiceLive } from '$lib/services/desktop/fs';
import {
	type CpalRecordingParams,
	RecorderError,
	type RecorderService,
} from '$lib/services/recorder/types';
import {
	asDeviceIdentifier,
	type Device,
	type DeviceAcquisitionOutcome,
} from '$lib/services/recorder/types';

/**
 * Audio recording data returned from the Rust method
 */
type AudioRecording = {
	sampleRate: number;
	channels: number;
	durationSeconds: number;
	filePath?: string;
};

/**
 * Enumerates available recording devices from the system.
 */
const enumerateDevices = async (): Promise<Result<Device[], RecorderError>> => {
	const { data: deviceNames, error: enumerateRecordingDevicesError } =
		await invoke<string[]>('enumerate_recording_devices');
	if (enumerateRecordingDevicesError) {
		return RecorderError.EnumerateDevices({
			cause: enumerateRecordingDevicesError,
		});
	}
	// On desktop, device names serve as both ID and label
	return Ok(
		deviceNames.map((name) => ({
			id: asDeviceIdentifier(name),
			label: name,
		})),
	);
};

/**
 * CPAL recorder service that uses the Rust CPAL method.
 * This service handles device enumeration, recording start/stop operations, and file management
 * for desktop audio recording using the CPAL library.
 */
export const CpalRecorderServiceLive: RecorderService = {
	/**
	 * Gets the current state of the recorder.
	 */
	getRecorderState: async (): Promise<
		Result<WhisperingRecordingState, RecorderError>
	> => {
		const { data: recordingId, error: getRecorderStateError } = await invoke<
			string | null
		>('get_current_recording_id');
		if (getRecorderStateError)
			return RecorderError.GetStateFailed({
				cause: getRecorderStateError,
			});

		return Ok(recordingId ? 'RECORDING' : 'IDLE');
	},

	enumerateDevices,

	/**
	 * Starts a recording session with the specified parameters.
	 * Handles device selection, fallback logic, and recording initialization.
	 *
	 * @param params - Recording parameters including device ID, recording ID, output folder, and sample rate
	 * @param callbacks - Callback functions for status updates
	 */
	startRecording: async (
		{
			selectedDeviceId,
			recordingId,
			outputFolder,
			sampleRate,
			streaming,
		}: CpalRecordingParams,
		{ sendStatus },
	): Promise<Result<DeviceAcquisitionOutcome, RecorderError>> => {
		const { data: devices, error: enumerateError } = await enumerateDevices();
		if (enumerateError) return Err(enumerateError);

		/**
		 * Acquires a recording device, either the selected one or a fallback.
		 */
		const acquireDevice = (): Result<
			DeviceAcquisitionOutcome,
			RecorderError
		> => {
			const deviceIds = devices.map((d) => d.id);
			const fallbackDeviceId = deviceIds.at(0);
			if (!fallbackDeviceId) {
				return RecorderError.NoDevice({
					message: selectedDeviceId
						? "We couldn't find the selected microphone. Make sure it's connected and try again!"
						: "We couldn't find any microphones. Make sure they're connected and try again!",
				});
			}

			if (!selectedDeviceId) {
				sendStatus({
					title: '🔍 No Device Selected',
					description:
						"No worries! We'll find the best microphone for you automatically...",
				});
				return Ok({
					outcome: 'fallback',
					reason: 'no-device-selected',
					deviceId: fallbackDeviceId,
				});
			}

			// Check if the selected device exists in the devices array
			const deviceExists = deviceIds.includes(selectedDeviceId);

			if (deviceExists)
				return Ok({ outcome: 'success', deviceId: selectedDeviceId });

			sendStatus({
				title: '⚠️ Finding a New Microphone',
				description:
					"That microphone isn't available. Let's try finding another one...",
			});

			return Ok({
				outcome: 'fallback',
				reason: 'preferred-device-unavailable',
				deviceId: fallbackDeviceId,
			});
		};

		const { data: deviceOutcome, error: acquireDeviceError } = acquireDevice();
		if (acquireDeviceError) return Err(acquireDeviceError);

		// Use the device from the outcome
		const deviceIdentifier = deviceOutcome.deviceId;

		// Now initialize recording with the chosen device
		sendStatus({
			title: '🎤 Setting Up',
			description:
				'Initializing your recording session and checking microphone access...',
		});

		// Convert sample rate string to number if provided
		const sampleRateNum = sampleRate
			? Number.parseInt(sampleRate, 10)
			: undefined;

		const { error: initRecordingSessionError } = await invoke(
			'init_recording_session',
			{
				deviceIdentifier,
				recordingId,
				outputFolder,
				sampleRate: sampleRateNum,
				streaming,
			},
		);
		if (initRecordingSessionError)
			return RecorderError.InitFailed({
				cause: initRecordingSessionError,
			});

		sendStatus({
			title: '🎙️ Starting Recording',
			description:
				'Recording session initialized, now starting to capture audio...',
		});
		const { error: startRecordingError } =
			await invoke<void>('start_recording');
		if (startRecordingError)
			return RecorderError.StartFailed({ cause: startRecordingError });

		return Ok(deviceOutcome);
	},

	/**
	 * Stops the current recording session and returns the recorded audio as a Blob.
	 * Handles file reading, session cleanup, and resource management.
	 *
	 * @param callbacks - Callback functions for status updates
	 */
	stopRecording: async ({
		sendStatus,
	}): Promise<Result<Blob, RecorderError>> => {
		const { data: audioRecording, error: stopRecordingError } =
			await invoke<AudioRecording>('stop_recording');
		if (stopRecordingError) {
			return RecorderError.StopFailed({ cause: stopRecordingError });
		}

		const { filePath } = audioRecording;
		// Desktop recorder should always write to a file
		if (!filePath) {
			return RecorderError.NoFilePath();
		}
		// audioRecording is now AudioRecordingWithFile

		// Read the WAV file from disk
		sendStatus({
			title: '📁 Reading Recording',
			description: 'Loading your recording from disk...',
		});

		const { data: blob, error: readRecordingFileError } =
			await FsServiceLive.pathToBlob(filePath);
		if (readRecordingFileError)
			return RecorderError.ReadFileFailed({
				cause: readRecordingFileError,
			});
		// Close the recording session after stopping
		sendStatus({
			title: '🔄 Closing Session',
			description: 'Cleaning up recording resources...',
		});
		const { error: closeError } = await invoke<void>('close_recording_session');
		if (closeError) {
			// Log but don't fail the stop operation
			console.error('Failed to close recording session:', closeError);
		}

		return Ok(blob);
	},

	/**
	 * Cancels the current recording session and cleans up resources.
	 * Deletes any temporary recording files and closes the recording session.
	 *
	 * @param callbacks - Callback functions for status updates
	 */
	cancelRecording: async ({
		sendStatus,
	}): Promise<Result<CancelRecordingResult, RecorderError>> => {
		// Check current state first
		const { data: recordingId, error: getRecordingIdError } = await invoke<
			string | null
		>('get_current_recording_id');
		if (getRecordingIdError) {
			return RecorderError.GetStateFailed({
				cause: getRecordingIdError,
			});
		}

		if (!recordingId) {
			return Ok({ status: 'no-recording' });
		}

		sendStatus({
			title: '🛑 Cancelling',
			description:
				'Safely stopping your recording and cleaning up resources...',
		});

		// First get the recording data to know if there's a file to delete
		const { data: audioRecording } =
			await invoke<AudioRecording>('stop_recording');

		// If there's a file path, delete the file using Tauri FS plugin
		if (audioRecording?.filePath) {
			const { filePath } = audioRecording;
			const { error: removeError } = await tryAsync({
				try: () => remove(filePath),
				catch: (error) => RecorderError.FileDeleteFailed({ cause: error }),
			});
			if (removeError)
				sendStatus({
					title: '❌ Error Deleting Recording File',
					description:
						"We couldn't delete the recording file. Continuing with the cancellation process...",
				});
		}

		// Close the recording session after cancelling
		sendStatus({
			title: '🔄 Closing Session',
			description: 'Cleaning up recording resources...',
		});
		const { error: closeError } = await invoke<void>('close_recording_session');
		if (closeError) {
			// Log but don't fail the cancel operation
			console.error('Failed to close recording session:', closeError);
		}

		return Ok({ status: 'cancelled' });
	},
};

/**
 * Wrapper function for Tauri invoke calls that handles errors consistently.
 * Converts Tauri invoke calls into Result types for better error handling.
 *
 * @param command - The Tauri command to invoke
 * @param args - Optional arguments to pass to the command
 */
async function invoke<T>(command: string, args?: Record<string, unknown>) {
	return tryAsync({
		try: async () => await tauriInvoke<T>(command, args),
		catch: (error) => RecorderError.InvokeFailed({ command, cause: error }),
	});
}
