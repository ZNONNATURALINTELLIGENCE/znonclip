# ZnonClip launch film

[`znonclip-launch-1080p.mp4`](znonclip-launch-1080p.mp4): 54.4 s, 1920 x 1080, 30 fps, H.264 + AAC, with sound.
[`znonclip-launch-voiceover-1080p.mp4`](znonclip-launch-voiceover-1080p.mp4): the same cut with a narrated voiceover (Kokoro-82M, voice
`af_heart`, generated locally; the music dips under each line).
Made with [HyperFrames](https://github.com/heygen-com/hyperframes) on 2026-10-24.

Every ZnonClip screen in the film is the real app rendering itself, through its `ZNONCLIP_SNAPSHOT` mode, with
demo clips. The terminal lines are real output:
- the receipt's SHA-256 matches `shasum -a 256`;
- `znonclip-cli scrub` redacts AWS's public example key.

Every claim comes from the [main README](../README.md), except one. "9 µs to predict your next paste" is the
median of 2,000 release-mode runs of the open-path ranking code (paste-stats query + `predict()`, 40 clips) on
Apple silicon. It was measured on 2026-10-23 with a temporary test that was not committed.

Before it shipped, the film was reviewed and scored 10/10 by two non-Claude judges: Gemini 4 Argon and
METAMORPHIOUS (Meta Muse Spark).

## Licence notice for this folder

The video is **not** covered by the repository's MIT licence.

- **Music:** generated locally with Meta's MusicGen (`facebook/musicgen-small`), whose weights are licensed
  [CC BY-NC 4.0](https://huggingface.co/facebook/musicgen-small) (non-commercial).
- **Sound effects:** from [Pixabay](https://pixabay.com/sound-effects/), used under the
  [Pixabay Content License](https://pixabay.com/service/license-summary/).
- **Voiceover:** [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache 2.0), voice `af_heart`.
- **Fonts:** Inter and JetBrains Mono (SIL Open Font License).
