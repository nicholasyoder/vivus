# Vision and principles

*Vivus* is Latin for "alive" or "living": the active, responsive part of the
desktop alongside Forest and Biome.

## Vision

Talk to your Linux desktop the way you'd talk to a capable person sitting next
to you, and get answers the way you'd expect them:

- **Voice in, voice out, for short things.** "What time is it?" gets a spoken answer and nothing else.
- **Voice plus a sidebar, for detailed things.** "What's the latest AI news?" gets a short spoken line ("let me pull that up in your sidebar"), then a spoken summary once results arrive. The full detail appears in an always-available sidebar you can read and scroll through.
- **Control of normal apps through speech.** Launch, switch, close, and later operate inside apps.
- **Awareness of what's on screen.** "What's this error?" resolves to the window you mean (the active window first, gaze later) and reads its text.

## Principles

1. **Not tied to one compositor.** Use standard Wayland protocols (`ext-*`, then `wlr-*`) first. Anything missing gets a new, documented protocol that vivus defines and Biome implements as the reference; any other compositor can implement it too. wlroots-only support is acceptable where nothing standard exists (e.g. window activation).
2. **Standalone first, Forest second.** vivus must be fully usable without Forest. A future Forest plugin is only a bridge: a D-Bus client that renders the sidebar in Forest's style.
3. **Inference is configuration, not code.** Every model role points at whatever endpoint you configure. LLM roles default to remote inference for speed; speech (VAD, STT, TTS) defaults to local, because it runs continuously and needs streaming.
4. **Structured actions before clicking pixels.** Prefer app launching, compositor protocols, D-Bus, AT-SPI actions, CLIs and app APIs. Screenshot-and-click is the last fallback.
5. **Latency is a feature.** Time from the end of your speech to the first audio back matters more than model smarts for everyday requests. Stream everything; cancel instantly on interrupt.
6. **Light by default, scale up when it helps.** It must run on an ordinary laptop with no GPU. Bigger local or remote models are a config choice. When lightness and voice quality conflict, voice quality wins, but pick the smallest model that gets there.

## Related projects

None cover this whole scope: [Vigil](https://github.com/3L0935/Vigil) (offline
LLM assistant with tools), [whisrs](https://docs.rs/crate/whisrs/0.1.19)
(Wayland dictation), [Talon](https://talonvoice.com/docs/) (no Wayland support).
