<p align="center">
  <img width="140" src="apps/whispering/src-tauri/recorder-state-icons/studio_microphone.png" alt="Whispering">
</p>

<h1 align="center">Whispering</h1>
<p align="center">Press a shortcut, speak, get text wherever your cursor is.</p>

<p align="center">
  <a href="https://github.com/samiralibabic/whispering/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/samiralibabic/whispering?style=flat-square&label=release&color=brightgreen"></a>
  <a href="licenses/LICENSE-MIT"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-blue?style=flat-square"></a>
</p>

This is a community-maintained continuation of **Whispering 7**, the open-source dictation app by [Braden Wong](https://github.com/braden-w) and the [Epicenter](https://github.com/EpicenterHQ/epicenter) contributors. Upstream has rebuilt Whispering as a file-based voice-notes app without global shortcuts, push-to-talk, voice-activated recording, auto-paste, or on-device transcription. This repository keeps the 7.x dictation app working and releases it. It is not affiliated with Epicenter.

## What it does

- **Push-to-talk or toggle recording** from a global shortcut, in any app.
- **Voice-activated recording** that transcribes each time you pause.
- **Transcription on your computer** (Whisper on macOS and Linux, Parakeet everywhere) or through OpenAI, Groq, Deepgram, ElevenLabs, Mistral, and other providers.
- **Text lands at your cursor**, in the clipboard, or both.
- **Transformations** that rewrite a transcript with an LLM prompt before it is pasted.
- **Your data stays on your machine.** Nothing is sent anywhere except to the transcription and LLM providers you configure.

## Download

Get the files from the [latest release](https://github.com/samiralibabic/whispering/releases/latest).

| Platform              | File                        | Status   |
| --------------------- | --------------------------- | -------- |
| macOS (Apple Silicon) | `Whispering_*_aarch64.dmg`  | Tested   |
| macOS (Intel)         | `Whispering_*_x64.dmg`      | Untested |
| Windows               | `Whispering_*_x64-setup.exe` | Untested |
| Linux                 | `.AppImage`, `.deb`, `.rpm` | Untested |

The app updates itself from this repository's releases.

### First launch

The builds are not signed by Apple or Microsoft, so the operating system warns the first time:

- **macOS:** open **System Settings → Privacy & Security** and click **Open Anyway**, or run `xattr -dr com.apple.quarantine /Applications/Whispering.app`. Then grant Microphone and Accessibility access when asked. Accessibility is what lets Whispering paste at your cursor.
- **Windows:** in the SmartScreen dialog, click **More info → Run anyway**.
- **Linux:** make the AppImage executable with `chmod +x Whispering_*.AppImage`, or install the `.deb` or `.rpm`.

### Upgrading from upstream Whispering 7.11

Install over the old app. The app identity is unchanged, so settings and recordings carry over. On first launch, Whispering moves 7.11 settings, recordings, and transformations into a newer storage format. Back up your data folder first if your recordings matter to you (on macOS, `~/Library/Application Support/com.bradenwong.whispering`).

## Reporting problems

Open an [issue](https://github.com/samiralibabic/whispering/issues). Include your platform, the Whispering version (Settings), and what you expected to happen.

## Development

The app lives in [`apps/whispering`](apps/whispering) (SvelteKit and Tauri). `apps/api` and `packages/*` are shared Epicenter code that the app depends on.

```bash
bun install
cd apps/whispering
bun tauri dev
```

Building, the macOS toolchain workaround, and the release steps are in [`apps/whispering/README.md`](apps/whispering/README.md#build-the-executable-yourself).

## License

Whispering is MIT-licensed; see [`LICENSE`](LICENSE). The original copyright belongs to Braden Wong and the Epicenter contributors. `apps/api` and `packages/sync` are AGPL-3.0-or-later.
