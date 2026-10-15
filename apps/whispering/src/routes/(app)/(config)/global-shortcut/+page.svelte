<script lang="ts">
	import { Button } from '@epicenter/ui/button';
	import * as Empty from '@epicenter/ui/empty';
	import DesktopIcon from '@lucide/svelte/icons/laptop';
	import { ChromeWebStoreIcon } from '$lib/components/icons';
	import { WHISPERING_RELEASES_URL } from '$lib/constants/app';
</script>

<svelte:head> <title>Change Global Keyboard Shortcut</title> </svelte:head>

<Empty.Root class="flex-1">
	<Empty.Header>
		<Empty.Title
			class="scroll-m-20 text-4xl font-bold tracking-tight lg:text-5xl"
		>
			Change Your Global Keyboard Shortcut
		</Empty.Title>
		<Empty.Description class="lg:text-xl">
			You can access global keyboard shortcuts on the extension and desktop app.
		</Empty.Description>
	</Empty.Header>
	<Empty.Content>
		<div class="flex flex-col gap-2 md:flex-row">
			<Button
				tooltip="Check out the Chrome Extension"
				href={WHISPERING_RELEASES_URL}
				target="_blank"
				rel="noopener noreferrer"
				variant="default"
				size="lg"
			>
				<DesktopIcon class="size-6" />
				Download for Desktop
			</Button>
			<Button
				tooltip="Check out the desktop app"
				href="https://chromewebstore.google.com/detail/whispering/oilbfihknpdbpfkcncojikmooipnlglo"
				target="_blank"
				rel="noopener noreferrer"
				variant="outline"
				size="lg"
			>
				<ChromeWebStoreIcon class="size-6" />
				Get Chrome Extension
			</Button>
		</div>
		<Button onclick={() => window.history.back()} variant="ghost"
			>Go back</Button
		>
	</Empty.Content>
</Empty.Root>
