# The walkthrough video

A narrated walkthrough of Glasshouse for people deciding whether to use
it: the desktop, an agent asking first, undo, the record, IT's console
(rules per team, apps turned off), Glasshouse Agents in the buyer's own
AWS, and why an enterprise can say yes.

```sh
tools/video/shots.sh                  # the pictures, into build/video/shots
tools/video/make.py                   # the video: build/video/glasshouse-walkthrough.mp4
tools/video/make.py --only tiers,why  # a few scenes, to check a change
```

- `walkthrough.json` is the script: each scene's words, the title on
  screen, where its pictures came from (shown on the scene), and which
  pictures.
- `shots.sh` takes the pictures, each from where it says:
  - the desktop shell's own screens, with sample data
    (`make shell-preview`);
  - the admin console, live on its fixture;
  - Glasshouse Agents on this machine, with Claude Haiku doing the work
    (`GLASS_ANTHROPIC_API_KEY`; about a tenth of a cent a task);
  - the dev VM for undo and the record (`make vm-up vm-deploy` first);
  - the test suites' logs.
- `make.py` narrates each scene with piper and the en_US "joe" voice,
  which was trained on CC0 recordings. It shows each scene's pictures
  slowly closer and fades between scenes. The narration is burned in as
  subtitles, and the `.srt` is kept beside the video. Narration is cached
  by its words, so changing one scene re-records only that scene.
- The narrator, a development tool like cfn-lint, never ships. Install it
  into `build/tools/piper`:

  ```sh
  python3 -m venv build/tools/piper
  build/tools/piper/bin/pip install "piper-tts>=1.3"
  mkdir -p build/tools/piper/voices && cd build/tools/piper/voices
  for f in en_US-joe-medium.onnx en_US-joe-medium.onnx.json; do
    curl -sSLO "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/joe/medium/$f"
  done
  ```
