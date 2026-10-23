# Meet's test recordings

`hey-glass.wav`: 16-bit mono at 22,050 Hz, 8.5 s. Two sentences, 1.2 s
apart, with 0.6 s of silence before and 0.8 s after:

1. "I'll send the deck after this call."
2. "Hey Glass, find the ABC report and send it to Susan."

Synthetic speech, made with Piper (MIT) and its `en_US-ljspeech-medium`
voice, trained on the LJ Speech dataset (public domain): each sentence
synthesized on its own, then joined with silence. No person's voice is in
it. Used by `meet/test/transcribe.test.ts` (the speech model on it) and
`e2e/meet.e2e.ts` (the engine's fake microphone plays it,
`--use-file-for-fake-audio-capture`).
