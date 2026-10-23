// Built-in presets for audio conversions. A preset names a target format and the option values
// to set; options the target does not have are skipped by the caller.
export const AUDIO_PRESETS = [
  { id: "podcast", name: "Podcast (Opus 64 kbps mono, −16 LUFS)", target: "opus",
    options: { bitrate: 64, channels: 1, normalize: -16 } },
  { id: "voice", name: "Voice memo (Opus 24 kbps mono)", target: "opus",
    options: { bitrate: 24, channels: 1 } },
  { id: "music-small", name: "Music, small (Opus 128 kbps)", target: "opus",
    options: { bitrate: 128 } },
  { id: "music-compat", name: "Music, compatible (MP3 192 kbps)", target: "mp3",
    options: { bitrate: 192 } },
  { id: "streaming", name: "Streaming (Ogg Vorbis, −14 LUFS)", target: "ogg",
    options: { quality: 5, normalize: -14 } },
  { id: "web", name: "Web audio (WebM Opus 96 kbps)", target: "weba",
    options: { bitrate: 96 } },
  { id: "archive", name: "Lossless archive (FLAC level 8)", target: "flac",
    options: { level: 8 } },
  { id: "apple-lossless", name: "Apple Lossless (M4A)", target: "m4a", options: {} },
  { id: "cd", name: "CD quality (WAV 16-bit 44.1 kHz)", target: "wav",
    options: { sample_rate: 44100, bit_depth: "16" } },
];

// The options of `preset` that `schema` (a target's write_options) accepts.
export function presetOptions(preset, schema) {
  const out = {};
  for (const [k, v] of Object.entries(preset.options)) if (schema && k in schema) out[k] = v;
  return out;
}
