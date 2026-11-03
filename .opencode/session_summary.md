# Session Summary — File Converter

## Objective
Keep extending the local "File Converter" app (PySide6 GUI + CLI + Dolphin service menu) at
`/home/aqua/Documents/Files/File Converter` (CachyOS Plasma 6, realtime `fileconverter` launch → offscreen CLI test at `/tmp/opencode/fct`).

Feature requests delivered:
1. Draggable area/panel splitters + compression controls (bitrate / sample rate / max file size).
2. Keep ALL metadata + album cover art through conversions.
3. Drop the `.converted` suffix from output names.
4. Type-in **Max File Size** limit on EVERY type (audio, video, image, PDF, document); `20MB`, `10MB` style input.
5. Size units are now **decimal MB by default**; image fitting keeps the **highest quality** and lands as close **just under the limit** as possible (binary search, not coarse jumps).

## Important Details
- Repo: `/home/aqua/Documents/Files/File Converter`, package `src/fileconverter/`; repo-root `fileconverter` symlinked to `~/.local/bin/fileconverter` (`os.path.realpath(__file__)`).
- Format matrix (unified target combo): audio→`flac,aac,ogg,mp3,wav,m4a,opus`; video→`webm,mkv,mp4,ogv,avi,gif`+audio; image→png/jpg/ico/webp/avif/pdf; document→pdf+images.
- **Units:** internal `max_size` is **decimal MB** (1e6 bytes). `_parse_max_size`: bare number → MB; `MB`/`GB` = 10^6/10^9; `MiB`/`GiB` = binary, converted ÷1e6 (so `1024 MiB`→`1073.741824` MB, `2 GiB`→`2147.483648` — unit conversion is intentional, not a bug). `_max_size_text` shows `:g` MB, exact GB multiples as `N GB`. CLI/labels/notes all say "MB".
- **libopus:** clamps bitrate to 256 kbps (mono stream), accepts only 8/12/16/24/48 kHz input → `_libopus_rate()` rounds (22050→24000, 32000→24000).
- **Covers** = attached-picture video stream; `probe.attached_picture()` must use `-show_streams -of json` (ffprobe silently omits `disposition` under `-show_entries stream=...`). Covers kept only for `_COVER_CAPABLE={"mp3","m4a","flac"}`; ogg/opus/wav **cannot** hold covers (ogg muxer aborts). mp3 needs `-id3v2_version 3`. All cmds carry `-map_metadata 0 -map_chapters 0`.
- Output naming: `name.ext`, `_unique_path` appends `.1`,`..N` (never overwrites).
- **Sizing math:**
  - `_size_limit_bytes` = `max_size * 1e6`.
  - `_size_limited_kbps` = `(max_size*1e6*8)/(seconds*1000)`; audio-only = whole budget, video = minus 192 kbps audio reserve (floored 32/100).
  - Images: `_magick_fit` (below).
  - PDF: img2pdf (if present, else magick fallback) → `_shrink_pdf()` via Ghostscript `/ebook` → `/screen` (gs 10.08.0 installed).
- **`_magick_fit` v2 (quality-first):** first encode at `base_quality`(=options quality, default 95) + `base_scale`. If ≤limit → done (max quality kept). Else **binary search** quality in `[20, base_quality]` at full scale for the **highest** quality ≤limit (re-encode best_q); only if even quality 20 can't fit (or lossless/`pdf` targets — `quality_capable` = `{jpg,jpeg,webp,avif}`), binary search **scale** in `[25, base_scale]` for the **largest** scale that fits at q_final (=20 for capable, else base_quality), best-effort `25%` if none. ~≤10 encodes worst case; verified noisy.png 3000x2250 in ~350 ms.

## Verified (current binary)
- 35 MB noisy.png 3000x2250 16-bit:
  - →jpg @20 MB = 5.5 MiB (q95 fits → **max quality kept**, correct)
  - →jpg @1 MB = **935.9 KiB** (binary search lands near-limit)
  - →webp @5 MB 2.8 MiB, →avif @8 MB 4.9 MiB (natural fit)
- tone.wav→opus `[≤1 MB]` 157 KiB (clamp intact), →flac `[≤2 MB]` 73.8 KiB; clip.mp4→webm `[≤1 MB]` 397.9 KiB; odt→pdf `[≤0.5 MB]`; noisy.png→pdf `[≤0.1 MB]` 37.7 KiB (gs).
- `_parse_max_size`: `""`→None, `20`/`0.06`→decimal MB, `500 MB`=500, `1.5 GB`=1500, `1024 MiB`=1073.741824, `2 GiB`=2147.483648.
- GUI round-trip: type `20`→collect=`20`; populate 1500→"1500", 2000→"2 GB". Mixed-file (wav+mp4+png) → all target categories incl. audio/video/image/doc_holder visible; image→PDF uses image knobs.
- Compile `python3 -m py_compile src/fileconverter/*.py` clean after final edits.

## Current task state
- All edits applied and compile-clean; each feature verified via CLI+offscreen GUI in `/tmp/opencode/fct`.
- **Not done:** final real-Plasma/GUI visual pass by the user (offscreen only), no version bump requested.

## Next Move
1. Nothing pending code-wise. Report to user with the numbers above: units are MB (decimal) everywhere; fitting is now highest-quality-first and lands just under the limit (their 7.1MB case was the old −5 quality jump overshooting the 20MB target; a max-quality encode that already fits is kept as-is).
2. Ask them to re-test their own 27.4MB→20MB image in the real GUI (they may choose a target/codec; result should be old behavior ~7MB → currently either max-quality or just-under-limit).

## Relevant Files
- `src/fileconverter/engine.py` — `_magick_fit` (binary-search fit), `_size_limit_bytes`/`_size_limited_kbps` (MB=1e6), `_build_audio_cmd`/`_build_video_cmd` (metadata, covers, sample-rate), `_COVER_CAPABLE`, `_libopus_rate`, `_convert_to_pdf` (img2pdf→gs shrink), `Job.output_path` (no `.converted`).
- `src/fileconverter/probe.py` — `attached_picture()` (`-show_streams -of json`).
- `src/fileconverter/gui.py` — QSplitter, `_make_max_size_edit`/`_parse_max_size`/`_max_size_text`/`_collect_max_size` (MB units), doc_holder, labels "MB (blank = no limit)", `_quality_note` `[≤ X MB]`.
- `src/fileconverter/cli.py` — `--max-size MB`, note `[≤ X MB]` (`:g`).
- `src/fileconverter/presets.py` — audio defaults (`sample_rate: None`); `src/fileconverter/matrix.py`.
- `fileconverter` launcher, `install.sh` (optional ghostscript hint), `fileconverter.desktop` (prior Dolphin work).
- Test media: `/tmp/opencode/fct/` — `noisy.png` (35 MB), `tone.wav`, `clip.mp4`, `jw.jpg`, `note.odt` (recreated after /tmp wipe).