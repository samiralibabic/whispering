<script lang="ts">
	import { Button } from '@epicenter/ui/button';
	import * as Card from '@epicenter/ui/card';
	import * as SectionHeader from '@epicenter/ui/section-header';
	import CommandIcon from '@lucide/svelte/icons/command';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import MicIcon from '@lucide/svelte/icons/mic';
	import ZapIcon from '@lucide/svelte/icons/zap';
	import { ChromeWebStoreIcon } from '$lib/components/icons';
	import { WHISPERING_RELEASES_URL } from '$lib/constants/app';
</script>

<svelte:head> <title>Download Desktop App - Whispering</title> </svelte:head>

<main
	class="flex flex-1 items-center justify-center p-4 container max-w-4xl mx-auto"
>
	<div class="w-full">
		<SectionHeader.Root class="text-center space-y-4 mb-8">
			<SectionHeader.Title
				level={1}
				class="scroll-m-20 text-4xl tracking-tight lg:text-5xl"
			>
				Whispering Desktop
			</SectionHeader.Title>
			<SectionHeader.Description class="text-lg lg:text-xl max-w-2xl mx-auto">
				Experience the full power of Whispering with native performance and
				system-wide integration.
			</SectionHeader.Description>
		</SectionHeader.Root>

		<div class="grid gap-6 mb-8 lg:grid-cols-3">
			<Card.Root class="p-6">
				<Card.Header class="gap-2 p-0">
					<CommandIcon class="h-8 w-8 text-primary" />
					<Card.Title class="text-lg">Global Shortcuts</Card.Title>
					<Card.Description>
						Customizable global keyboard shortcuts for recording control from
						anywhere on your system
					</Card.Description>
				</Card.Header>
			</Card.Root>

			<Card.Root class="p-6">
				<Card.Header class="gap-2 p-0">
					<MicIcon class="h-8 w-8 text-primary" />
					<Card.Title class="text-lg">Native Performance</Card.Title>
					<Card.Description>
						Rust backend, more audio APIs, and optimized resource usage
					</Card.Description>
				</Card.Header>
			</Card.Root>

			<Card.Root class="p-6">
				<Card.Header class="gap-2 p-0">
					<ZapIcon class="h-8 w-8 text-primary" />
					<Card.Title class="text-lg">System Integration</Card.Title>
					<Card.Description>
						System tray controls, direct text injection, auto-updates, and
						window management
					</Card.Description>
				</Card.Header>
			</Card.Root>
		</div>

		<div class="flex flex-col items-center gap-4">
			<div class="flex flex-col gap-3 sm:flex-row">
				<Button
					tooltip="Download Whispering Desktop App"
					href={WHISPERING_RELEASES_URL}
					target="_blank"
					rel="noopener noreferrer"
					variant="default"
					size="lg"
					class="min-w-[200px]"
				>
					<DownloadIcon class="size-5" />
					Download for Desktop
				</Button>
				<Button
					tooltip="Get the Chrome Extension"
					href="https://chromewebstore.google.com/detail/whispering/oilbfihknpdbpfkcncojikmooipnlglo"
					target="_blank"
					rel="noopener noreferrer"
					variant="outline"
					size="lg"
				>
					<ChromeWebStoreIcon class="size-5" />
					Chrome Extension
				</Button>
			</div>

			<Button
				onclick={() => window.history.back()}
				variant="ghost"
				class="mt-2"
			>
				Go back
			</Button>
		</div>
	</div>
</main>
