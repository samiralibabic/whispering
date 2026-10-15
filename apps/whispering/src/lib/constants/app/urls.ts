import { APP_URLS } from '@epicenter/constants/vite';

/**
 * URL and pathname constants for the Whispering application
 */
export const WHISPERING_URL = APP_URLS.AUDIO;

export const WHISPERING_URL_WILDCARD = `${WHISPERING_URL}/*` as const;

export const WHISPERING_RECORDINGS_PATHNAME = '/recordings' as const;

export const WHISPERING_SETTINGS_PATHNAME = '/settings' as const;

/**
 * GitHub repository that publishes this app's source and releases.
 * The updater endpoint in src-tauri/tauri.conf.json must point at the same repository.
 */
export const WHISPERING_GITHUB_REPO = 'samiralibabic/whispering' as const;

export const WHISPERING_GITHUB_URL =
	`https://github.com/${WHISPERING_GITHUB_REPO}` as const;

export const WHISPERING_RELEASES_URL =
	`${WHISPERING_GITHUB_URL}/releases` as const;
