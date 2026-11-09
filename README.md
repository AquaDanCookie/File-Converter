<p align="center">
  <img src="assets/fileconverter-128.png?v=2" width="112" alt="File Converter icon">
</p>

<h1 align="center">File Converter</h1>

<p align="center">
  A native Rust file converter for Linux with a GUI, a CLI, batch conversion,
  automatic quality matching, and strict output-size limits.
</p>

<p align="center">
  <img alt="Rust" src="https://emblem.xdan.cc/badge.svg?label=rust&amp;message=2021&amp;look=connected&amp;labelColor=%233D3D3D&amp;color=%23FDD017&amp;icon=rust">
  <img alt="Platform" src="https://emblem.xdan.cc/badge.svg?label=platform&amp;message=Linux&amp;look=connected&amp;labelColor=%233D3D3D&amp;color=%23FDD017&amp;icon=linux">
  <img alt="License" src="https://emblem.xdan.cc/badge.svg?label=license&amp;message=MIT&amp;look=connected&amp;labelColor=%233D3D3D&amp;color=%23FDD017&amp;icon=material-symbols:license">
</p>

<p align="center">
  <img src="screenshots/File-Converter.png?v=2" alt="File Converter main window">
</p>

---

## Features

- **Audio, video, image, and document conversion** in one app.
- **Native desktop GUI** built with `eframe`/`egui`, including drag and drop,
  file list, media information, progress, and cancellation.
- **Command-line interface** for scripts and headless conversions.
- **Batch processing** with configurable parallelism.
- **Automatic quality matching**:
  - Audio bitrate is matched to each source file.
  - WAV/PCM files work even when `ffprobe` omits the stream bitrate.
  - Video CRF and image quality can also follow the source.
- **Strict maximum file size**:
  - The final output is checked after encoding.
  - Bitrate, sample rate, image quality, or image scale are reduced as needed.
  - If the target cannot be reached, the conversion fails instead of leaving an
    oversized file.
- **Presets and defaults** stored in `~/.config/file-converter/presets.json`.
- **KDE/Dolphin integration** with a “Convert with File Converter…” context
  menu entry.
- **Application icon** integrated into the window, taskbar, and app menu.
- Metadata, chapters, and cover art are preserved where the target format
  supports them.

## Supported formats

| Category | Output formats |
| --- | --- |
| Audio | FLAC, AAC, OGG, MP3, WAV, M4A, OPUS |
| Video | WEBM, MKV, MP4, OGV, AVI, GIF |
| Image | PNG, JPG, ICO, WEBP, AVIF |
| Document | PDF |

Video files can also be converted to audio-only formats. Images and documents
can be converted to PDF, and documents can be converted to image formats.

Common inputs include:

- **Audio:** MP3, FLAC, WAV, M4A, OGG, OPUS, AAC, AIFF, WMA, and more.
- **Video:** MP4, MKV, WebM, MOV, AVI, WMV, FLV, MPEG, TS, VOB, GIF, and more.
- **Images:** PNG, JPEG, WebP, AVIF, TIFF, BMP, ICO, SVG, HEIC, PSD, and
  several RAW formats.
- **Documents:** PDF, DOC/DOCX, ODT, XLS/XLSX, ODS, PPT/PPTX, and ODP.

Run `fileconverter --list-formats` to print the current conversion matrix.

## Requirements

### Runtime tools

| Tool | Used for | Package examples |
| --- | --- | --- |
| `ffmpeg` / `ffprobe` | Audio and video conversion | `ffmpeg` |
| ImageMagick 7 (`magick`) | Image conversion and PDF fallback | `imagemagick` |
| LibreOffice (`soffice`) | Office document conversion | `libreoffice` |
| `img2pdf` | Optional image-to-PDF assembly | `img2pdf` |
| Ghostscript (`gs`) | PDF compression and PDF size limits | `ghostscript` |

On Arch Linux:

```bash
sudo pacman -S --needed ffmpeg imagemagick libreoffice img2pdf ghostscript
```

### Build requirements

- Current stable Rust toolchain with Cargo.
- Linux desktop development libraries required by `eframe`/`winit`.
- `pkg-config`.

Install Rust with [rustup](https://rustup.rs/) if needed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Installation

Clone the repository and run the installer:

```bash
git clone <repository-url>
cd <repository-folder>
./install.sh
```

The installer:

1. Builds the Rust release binary.
2. Installs it as `~/.local/bin/fileconverter`.
3. Installs the application entry and icon theme files.
4. Installs the Dolphin service menu.
5. Creates presets in `~/.config/file-converter/presets.json` if needed.
6. Refreshes KDE and desktop caches.

Make sure `~/.local/bin` is on your `PATH`.

### Build without installing

```bash
cargo build --release
./target/release/fileconverter
```

## Usage

### GUI

```bash
fileconverter
fileconverter song.wav
fileconverter *.mp4
```

Without `--to`, the GUI opens with the supplied files. You can also launch
**File Converter** from the application menu or use the Dolphin context menu.

### CLI

```bash
# Video to WebM
fileconverter clip.mp4 --to webm

# Audio to MP3, using automatic source-matched quality
fileconverter song.wav --to mp3

# Image conversion with explicit options
fileconverter photo.png --to jpg --scale 80 --quality 85

# Convert a video to an audio file
fileconverter recording.mkv --to mp3

# Office document to PDF
fileconverter report.docx --to pdf

# Fit the output under 20 MB
fileconverter movie.mp4 --to mp4 --max-size 20

# Put outputs in a different directory
fileconverter *.wav --to flac --outdir ~/Music/converted
```

### CLI options

| Option | Description |
| --- | --- |
| `--to <FORMAT>` | Target format. Without it, the GUI opens. |
| `--max-size <MB>` | Hard output limit in decimal megabytes. |
| `--bitrate <K>` | Audio bitrate in kbps. Defaults to auto. |
| `--sample-rate <HZ>` | Resample audio, for example `48000`. |
| `--crf <N>` | Video CRF from 0 to 51. Lower is better. Defaults to auto. |
| `--preset <NAME>` | Video encoder preset, from `ultrafast` to `veryslow`. |
| `--vbitrate <K>` | Video bitrate in kbps. Overrides CRF. |
| `--scale <PCT>` | Image scale percentage. |
| `--quality <Q>` | Image quality from 1 to 100. Defaults to auto. |
| `--outdir <DIR>` | Output directory. Defaults to the source directory. |
| `--jobs <N>` | Number of parallel conversions. Defaults to 2. |
| `--gui` | Force the GUI even when `--to` is supplied. |
| `--list-formats` | Print supported conversions and exit. |

Use `fileconverter --help` for the complete list.

## Automatic quality

The default audio setting is **Auto (match source)**. For each file, the app:

1. Reads the source audio stream bitrate with `ffprobe`.
2. Derives the bitrate for PCM/WAV sources from
   `sample rate × channels × bit depth` when `ffprobe` does not provide one.
3. Picks the closest quality the selected output format supports.

Examples:

| Source | Detected bitrate | Auto output |
| --- | --- | --- |
| 16-bit / 44.1 kHz stereo WAV | 1411 kbps | MP3 320 kbps |
| 24-bit / 48 kHz stereo WAV | 2304 kbps | MP3 320 kbps |
| 8-bit / 8 kHz mono WAV | 64 kbps | MP3 128 kbps |

Video and image quality can also use source-based automatic values. Because
quality is resolved per job, a batch can contain files with different source
bitrates.

## Maximum file size

`--max-size` and the GUI's **Max File Size** field use decimal megabytes:
`20` means 20,000,000 bytes.

The limit is enforced after encoding. If the first attempt is too large, the
app retries with a smaller bitrate, lower sample rate, lower image quality, or a
smaller image scale. If the output still cannot fit, the conversion fails and
the oversized output is removed.

Notes:

- Audio: lossy formats can be reduced automatically. Lossless **FLAC/WAV**
  output is accepted when it already fits; if it is over the limit, the app
  reports an error because lossless output cannot be shrunk by lowering a
  bitrate.
- Video: the total budget is split between video and audio. MP4, MKV, and WebM
  work with the size-fitting logic.
- Images: quality and scale are reduced as needed.
- PDF: Ghostscript is required to shrink PDFs. If it cannot reach the requested
  size, the conversion fails instead of leaving an oversized PDF.
- Extremely small targets for formats with codec minimums, such as some AVI or
  GIF outputs, may fail. The app never reports an oversized result as a
  successful conversion.

## Dolphin integration

After running `./install.sh`, select one or more files in Dolphin, right-click,
and choose:

```text
Convert with File Converter…
```

If the entry does not appear, restart Dolphin. The service menu uses the
installed `~/.local/bin/fileconverter` binary.

## Presets

Presets and category defaults live in:

```text
~/.config/file-converter/presets.json
```

The file contains defaults for audio, video, image, and document conversions,
plus user presets. `null` means automatic source matching. Edit the file and
press **Reload** in the GUI to apply changes.

To regenerate the default file:

```bash
fileconverter --write-default-presets
```

## Project layout

```text
src/
  main.rs       Application entry point
  cli.rs        Command-line interface and headless conversion
  gui.rs        egui/eframe desktop interface
  engine.rs     Conversion engine, quality matching, size enforcement
  matrix.rs     Input detection and conversion matrix
  presets.rs    Preset loading, migration, and saving
  probe.rs      ffprobe-based media inspection
assets/         Application icons
fileconverter-app.desktop   Application menu entry
fileconverter.desktop       Dolphin service menu
install.sh      Build and install into the user account
uninstall.sh    Remove the installed application
```

The application itself is implemented entirely in Rust. `install.sh` and
`uninstall.sh` are packaging helpers.

## Development

```bash
cargo fmt --check
cargo check
cargo test
cargo build --release
```

Install the current build locally:

```bash
./install.sh
```

Remove it:

```bash
./uninstall.sh
```

`uninstall.sh` keeps `~/.config/file-converter` so presets are not lost.

## Notes

- The app is developed for Linux and integrates with KDE Plasma/Dolphin.
  The GUI itself runs on Wayland and X11. It has been tested on Arch Linux
  with KDE Plasma 6 on Wayland.
- `img2pdf` is optional; ImageMagick is used as a fallback when it is absent.
- Ghostscript is optional for general PDF conversion but required for PDF size
  limits.
- Office document conversion depends on LibreOffice.
- If a conversion cannot satisfy a requested size limit, it exits with an error
  rather than writing an oversized file.

## License

MIT
