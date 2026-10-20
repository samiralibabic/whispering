import { AutostartServiceLive } from './autostart';
import { CommandServiceLive } from './command';
import { DictationServiceLive } from './dictation';
import { FfmpegServiceLive } from './ffmpeg';
import { FsServiceLive } from './fs';
import { GlobalShortcutManagerLive } from './global-shortcut-manager';
import { PermissionsServiceLive } from './permissions';
import { CpalRecorderServiceLive } from './recorder/cpal';
import { FfmpegRecorderServiceLive } from './recorder/ffmpeg';
import { TrayIconServiceLive } from './tray';

/**
 * Desktop-only services.
 * These services are only available in the Tauri desktop app.
 */
export const desktopServices = {
	autostart: AutostartServiceLive,
	command: CommandServiceLive,
	dictation: DictationServiceLive,
	ffmpeg: FfmpegServiceLive,
	fs: FsServiceLive,
	tray: TrayIconServiceLive,
	globalShortcutManager: GlobalShortcutManagerLive,
	permissions: PermissionsServiceLive,
	cpalRecorder: CpalRecorderServiceLive,
	ffmpegRecorder: FfmpegRecorderServiceLive,
} as const;
