<script module lang="ts">
	import type { Update } from '@tauri-apps/plugin-updater';

	export const updateDialog = createUpdateDialog();
	export type UpdateInfo = Pick<
		Update,
		'version' | 'date' | 'body' | 'downloadAndInstall'
	> | null;

	function createUpdateDialog() {
		let isOpen = $state(false);
		let update = $state<UpdateInfo | null>(null);
		let downloadProgress = $state(0);
		let downloadTotal = $state(0);
		let error = $state<string | null>(null);

		return {
			get isOpen() {
				return isOpen;
			},
			set isOpen(v) {
				isOpen = v;
			},
			get update() {
				return update;
			},
			get isDownloading() {
				return downloadTotal > 0 && downloadProgress < downloadTotal && !error;
			},
			get isDownloadComplete() {
				return downloadTotal > 0 && downloadProgress >= downloadTotal && !error;
			},
			get progressPercentage() {
				return downloadTotal > 0 ? (downloadProgress / downloadTotal) * 100 : 0;
			},
			get error() {
				return error;
			},
			open(newUpdate: UpdateInfo) {
				update = newUpdate;
				isOpen = true;
				downloadProgress = 0;
				downloadTotal = 0;
				error = null;
			},
			close() {
				isOpen = false;
			},
			updateProgress(progress: number, total: number) {
				downloadProgress = progress;
				downloadTotal = total;
			},
			setError(err: string | null) {
				error = err;
				downloadTotal = 0;
			},
		};
	}
</script>

<script lang="ts">
	import * as Alert from '@epicenter/ui/alert';
	import { Button } from '@epicenter/ui/button';
	import * as Dialog from '@epicenter/ui/dialog';
	import { Link } from '@epicenter/ui/link';
	import { Progress } from '@epicenter/ui/progress';
	import { ScrollArea } from '@epicenter/ui/scroll-area';
	import { Separator } from '@epicenter/ui/separator';
	import AlertTriangleIcon from '@lucide/svelte/icons/alert-triangle';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import { relaunch } from '@tauri-apps/plugin-process';
	import DOMPurify from 'dompurify';
	import { marked } from 'marked';
	import { extractErrorMessage } from 'wellcrafted/error';
	import { Err, tryAsync } from 'wellcrafted/result';
	import { WHISPERING_RELEASES_URL } from '$lib/constants/app';
	import { rpc } from '$lib/query';

	function getGitHubReleaseUrl(version: string) {
		const tag = version.startsWith('v') ? version : `v${version}`;
		return `${WHISPERING_RELEASES_URL}/tag/${tag}`;
	}

	function renderMarkdown(markdown: string): string {
		const html = marked.parse(markdown) as string;
		return DOMPurify.sanitize(html);
	}

	async function handleDownloadAndInstall() {
		if (!updateDialog.update) return;

		updateDialog.setError(null);

		let downloaded = 0;
		let contentLength = 0;

		const { error } = await tryAsync({
			try: () =>
				updateDialog.update!.downloadAndInstall((event) => {
					switch (event.event) {
						case 'Started':
							contentLength = event.data.contentLength ?? 0;
							updateDialog.updateProgress(0, contentLength);
							break;
						case 'Progress':
							downloaded += event.data.chunkLength;
							updateDialog.updateProgress(downloaded, contentLength);
							break;
						case 'Finished':
							rpc.notify.success({
								title: 'Update installed successfully!',
								description: 'Restart Whispering to apply the update.',
								action: {
									type: 'button',
									label: 'Restart Whispering',
									onClick: () => relaunch(),
								},
							});
							break;
					}
				}),
			catch: (error) => Err(extractErrorMessage(error)),
		});
		if (error) {
			updateDialog.setError(error);
			rpc.notify.error({
				title: 'Failed to install update',
				description: error,
			});
		}
	}
</script>

<Dialog.Root bind:open={updateDialog.isOpen}>
	<Dialog.Content class="sm:max-w-lg">
		<Dialog.Header>
			<Dialog.Title>Update Available</Dialog.Title>
			<Dialog.Description>
				Version {updateDialog.update?.version} is ready to install
				{#if updateDialog.update?.date}
					&middot; {new Date(updateDialog.update.date).toLocaleDateString()}
				{/if}
				{#if updateDialog.update?.version}
					&middot;
					<Link
						href={getGitHubReleaseUrl(updateDialog.update.version)}
						target="_blank"
						rel="noopener noreferrer"
					>
						View release notes
					</Link>
				{/if}
			</Dialog.Description>
		</Dialog.Header>

		{#if updateDialog.update?.body}
			<ScrollArea class="max-h-[300px]">
				<div class="prose prose-sm max-w-none pr-4">
					{@html renderMarkdown(updateDialog.update.body)}
				</div>
			</ScrollArea>
			<Separator />
		{/if}

		{#if updateDialog.isDownloading || updateDialog.isDownloadComplete}
			<div class="space-y-2">
				<div
					class="flex items-center justify-between text-sm text-muted-foreground"
				>
					<span>
						{updateDialog.isDownloadComplete
							? 'Download complete'
							: 'Downloading...'}
					</span>
					<span class="tabular-nums">
						{Math.round(updateDialog.progressPercentage)}%
					</span>
				</div>
				<Progress value={updateDialog.progressPercentage} max={100} />
			</div>
		{/if}

		{#if updateDialog.error}
			<Alert.Root variant="destructive">
				<AlertTriangleIcon />
				<Alert.Title>Installation failed</Alert.Title>
				<Alert.Description> {updateDialog.error} </Alert.Description>
			</Alert.Root>
		{/if}

		<Dialog.Footer>
			<Button
				variant="outline"
				onclick={() => updateDialog.close()}
				disabled={updateDialog.isDownloading}
			>
				Later
			</Button>
			{#if updateDialog.isDownloadComplete}
				<Button onclick={() => relaunch()}>Restart Now</Button>
			{:else}
				<Button
					onclick={handleDownloadAndInstall}
					disabled={updateDialog.isDownloading}
				>
					{#if updateDialog.isDownloading}
						Downloading...
					{:else}
						<DownloadIcon />
						Install Update
					{/if}
				</Button>
			{/if}
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
