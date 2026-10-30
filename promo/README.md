# ZnonClip launch film

[`znonclip-launch-1080p.mp4`](znonclip-launch-1080p.mp4): 54.4 s, 1920 x 1080, 30 fps, H.264 + AAC, with sound.
[`znonclip-launch-voiceover-1080p.mp4`](znonclip-launch-voiceover-1080p.mp4): the same cut with a narrated voiceover (Kokoro-82M, voice
`af_heart`, generated locally; the music dips under each line).
[`znonclip-launch-cyberpunk-1080p.mp4`](znonclip-launch-cyberpunk-1080p.mp4) is the same cut re-skinned as a cyberpunk
film. It has a neon perspective grid, glass windows, a holographic rim on the real panel, glitch cuts, and a dubstep
bed whose drop hits the ZnonClip reveal.
[`znonclip-launch-cyberpunk-vertical-1080x1350.mp4`](znonclip-launch-cyberpunk-vertical-1080x1350.mp4) is that cut in
4:5 for X. Each scene is re-laid out top to bottom, not cropped. The scrub command is split with a shell line
continuation (`\`), which is the same command.
Made with [HyperFrames](https://github.com/heygen-com/hyperframes) on 2026-10-24.

## CGI ad (Blender)

[`znonclip-cgi-ad-16x9.mp4`](znonclip-cgi-ad-16x9.mp4), [`-9x16`](znonclip-cgi-ad-9x16.mp4) and [`-1x1`](znonclip-cgi-ad-1x1.mp4) are a 12 s
luxury-style product spot, built in Blender 5.2 on 2026-10-29. It shows glass ⌃ ⌘ V keycaps with gold glyphs, 20 glass
clip cards stacking under a violet "suggested" card, and a smoked-glass slab carrying the real ZnonClip panel render.
It ends on a gold wordmark. Each format was rendered natively from its own framing, not cropped. Titles and sound were
cut in HyperFrames.

Gemini 4 Argon scored the final version 10/10 (after 6 and 8). METAMORPHIOUS did not score this one. It was pushed on
Zach's go with Argon's score alone.

Every ZnonClip screen in the film is the real app rendering itself, through its `ZNONCLIP_SNAPSHOT` mode, with
demo clips. The terminal lines are real output:
- the receipt's SHA-256 matches `shasum -a 256`;
- `znonclip-cli scrub` redacts AWS's public example key.

Every claim comes from the [main README](../README.md), except one. "9 µs to predict your next paste" is the
median of 2,000 release-mode runs of the open-path ranking code (paste-stats query + `predict()`, 40 clips) on
Apple silicon. It was measured on 2026-10-23 with a temporary test that was not committed.

Before they shipped, the film and its cyberpunk cut were each reviewed and scored 10/10 by two non-Claude judges:
Gemini 4 Argon and METAMORPHIOUS (Meta Muse Spark). For the vertical cut, Argon scored the final version 10/10.
METAMORPHIOUS scored the version just before it 10/10; the final version adds only Argon's four layout fixes.

## Licence notice for this folder

The video is **not** covered by the repository's MIT licence.

- **Music:** generated locally with Meta's MusicGen (`facebook/musicgen-small`), whose weights are licensed
  [CC BY-NC 4.0](https://huggingface.co/facebook/musicgen-small) (non-commercial). This covers the funky bed, the
  cyberpunk cut's dubstep bed and the CGI ad's bed.
- **Sound effects:** from [Pixabay](https://pixabay.com/sound-effects/), used under the
  [Pixabay Content License](https://pixabay.com/service/license-summary/). The cyberpunk cut's riser and sub-drop are
  synthesized; its glitch stabs are slices of a Pixabay effect.
- **Voiceover:** [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache 2.0), voice `af_heart`.
- **Fonts:** Inter, JetBrains Mono and, in the cyberpunk cut, Oswald (all SIL Open Font License).
- **CGI ad lighting:** the studio HDRI `studio_small_09` from [Poly Haven](https://polyhaven.com/a/studio_small_09) (CC0). All geometry,
  materials and lights are original, built in code.
