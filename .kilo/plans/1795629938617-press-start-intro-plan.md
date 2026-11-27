# Press Start Screen + Opening Movie + Title Voice

## Goal

Make the default `acvd-game` launch reproduce the 360 front end:

1. Title screen from `lang/jp/menu/title.drb.dcx` + `lang/jp/menu/title.tpf.dcx`, with the localized
   `PRESS START` text (menu FMG id 204 already known), title animation, BGM and any title voice cue.
2. On Start, play the 1280x720 `acv_opening` movie (`MOVIE_PLAY_INFO_ST` row 0) with its voice
   track.
3. Hand off to the existing 3D garage (`GarageScreen::Top`).

Not in scope: publisher/developer logos, the `IniSettings` first-run/welcome movie, the `prologue`
movie, the retail `MenuTop` STORY/VERSUS/SYSTEM screen, the other 22 movies, and a runtime VC-1
decoder.

## Locked decisions

- **Scope**: title screen -> `acv_opening` -> existing garage; this becomes the default launch.
- **Movie decode**: explicit `tools/acvd-movies` tool uses the local `ffmpeg`/`ffprobe` once to
  transcode `movie/jp/acv_opening.wmv` into a private frame + WAV cache under `private/`. No codec
  code and no external process at runtime.
- **Voice**: the movie's WMA Pro track is decoded by the cache tool; any separate title-screen
  voice/BGM cues are traced and played through the existing FEV/FSB pipeline.
- **Cache build**: explicit tool only. If the cache is missing at runtime, print the exact command
  and continue title -> garage without the FMV; never spawn ffmpeg from the game.
- **Bypass**: `--boot garage` / `--skip-boot` (and the existing debug flags) keep the current
  direct-to-garage/sortie paths for tests and demos.

## Current facts (verified in the workspace)

- `lang/jp/menu/title.drb.dcx` (4813 bytes) and `title.tpf.dcx` (495216 bytes) are the only title
  layout/texture pair; there is no `lang/en/menu/title.drb.dcx`. That strongly implies the 360 uses
  one layout for every language and localizes only the text (confirm with the runtime capture).
- `lang/en/text/menu/menu.fmg` id 204 is `PRESS START` (`fmg.rs::tests::disc_banks`). Bank 1 is the
  menu FMG; `acvd-render::menu` already resolves `TextSource::Message { bank: 1, id }` from
  `lang/<layout-lang>/text/menu/menu.fmg`.
- `crates/acvd-data/src/generated/menu.rs` has `PARAM_MOVIEPLAYINFO_BIN`: row 0 is
  `オープニング` -> `acv_opening`, 1280x720, `play_type 0`; rows 10/11 are the `IniSettings`
  startup/welcome movies and row 100 is `prologue`.
- `sheets/formats.csv` `.wmv`: 23 ASF movies under `movie/jp/*.wmv`, WMV3 video, WMA2 stereo except
  `prologue`/`inisettings` which are WMA Pro 6ch at 1280x720.
- `sound::resolve_bgm_wave(disc, None)` already plays the front-end/garage theme `grg` from
  `sound/acv2_bgm_menu_bgm_menu.fev` + `sound/bgm_menu.fsb`. Title-specific cues still need tracing.
- `sound.rs` already loads `acv2_se_menu.fev` and `acv2_se_garage.fev` (UI bus) and decodes XMA/PCM16
  samples; `acv2_voice_voice.fev` and the BGM FEV are not yet in the cue projects.
- `acvd-render::menu` handles all decoded DRB shape classes (`Sprite`, `Mono*`, `Gouraud*`, `Text`,
  `Dialog`, `AlphaAnimSprite`); the decoded classes are 2D. Any unknown `Shape::Other` record in
  `title.drb` must be checked and decoded before drawing.
- `menu::load` derives the text language from the layout path, so loading the jp title would load
  the jp FMG. A language override is required to show English text on the jp layout.
- `main.rs` currently builds the map/AC/garage in `Startup` (`setup`, `weapons::setup`) and starts
  in `Mode::Garage` only with `--garage*`; sortie is the default. The front-end must run before that
  setup so the title appears immediately and the garage is not updated behind it.

## Open questions the trace must close

| # | Question | Where the answer comes from |
|---|---|---|
| Q1 | Does `acv_opening` play before the title, after Start, or after idling? | passive Xenia boot capture |
| Q2 | Exact scene class/function that owns the title, and its Update/Exit | exe strings + Ghidra |
| Q3 | Which FMG ids / static strings the title texts use | `drbdump` + `fmgdump` |
| Q4 | Title BGM cue name and whether a voice cue fires | Xenia cue probes + `fevdump` |
| Q5 | Exact input, blink/fade timings, and movie skip behavior | title Update decompile + shots |
| Q6 | Whether `title.drb` uses any undecoded shape class or runtime image slot | `drbdump` raw records |
| Q7 | Movie frame rate and duration for the cache | `ffprobe` |

Do not start Phase 4 until Q1-Q7 are answered or explicitly assumed in `sheets/boot_flow.csv` /
`sheets/movies.csv`.

## Phase 1 - Trace

Set the cargo target dir before every cargo command:

```powershell
$env:CARGO_TARGET_DIR = "$PWD\target"; $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
```

### T1. Build/refresh the fact index and find the front-end strings

```powershell
cargo build --release -p acvd-index
.\target\release\acvd-index.exe build
$x = ".\target\release\acvd-index.exe"
& $x q const title.drb --limit 40
& $x q const title --limit 40
& $x q const Title --limit 40
& $x q const AppMenuMoviePlayer --limit 40
& $x q const Scr_PlayBGMByCueName --limit 40
& $x q const movie --limit 40
& $x q func 0x824a8890
& $x q callers 0x824a8890
& $x q callers 0x82646580
& $x q func 0x82801168
& $x q callers 0x82801168
```

- `0x824a8890` / `0x82646580` / `0x82801168` are the known FMV surface / named render-target chain
  from the `o0006` work. Find the movie player's `Start(id)` and the title scene's calls into it.
- Use `q name` for `MOVIE_PLAY_INFO_ST` / `MoviePlayInfo` if the param lookup is needed.
- Record every 360 address used later in `sheets/boot_flow.csv`, `sheets/movies.csv` or
  `sheets/sound_cues.csv`.

### T2. Decompile the title scene and movie start

Use the resident Ghidra bridge (detach `start`; never inline):

```powershell
$g = "$PWD\external\ghidra-cli\ghidra.exe"
Start-Process $g start -WindowStyle Hidden -RedirectStandardOutput private\tmp\bridge.out -RedirectStandardError private\tmp\bridge.err
& $g decompile 0x824a8890
& $g decompile FUN_82646580
& $g x-ref to 0x824a8890
```

- Follow the title scene's `Initialize` / `Update` / `SceneExit` (or native equivalent) to get:
  layout asset path, text ids, BGM trigger, input handling, fade/blink timings, movie id, and the
  next scene/result.
- If Ghidra reports `bad instruction data` (VMX128), immediately switch to
  `py private\tmp\x360func.py ADDR` + `py private\tmp\x360dis.py LO HI`.
- Stop the bridge before any headless Ghidra script: `& $g stop`.

### T3. Recover the runtime order, timings and cues with Xenia

First run a **passive** recipe with no input, to see the real order. Add
`tools\xenia\recipes\boot_passive.txt`:

```
wait 10
shot private/xenia/shots/boot_10.png
wait 10
shot private/xenia/shots/boot_20.png
wait 10
shot private/xenia/shots/boot_30.png
wait 15
shot private/xenia/shots/boot_45.png
wait 15
shot private/xenia/shots/boot_60.png
wait 30
shot private/xenia/shots/boot_90.png
wait 30
shot private/xenia/shots/boot_120.png
```

Then the input recipe (commit it as `tools\xenia\recipes\boot_title.txt` once verified; `run`
resolves relative to the recipe file, so both files live in that folder). Trim the passive waits
after the passive run tells you when the title actually appears:

```
run boot_passive.txt
press START 120
wait 1
shot private/xenia/shots/title_start_01.png
wait 4
shot private/xenia/shots/intro_05.png
wait 5
shot private/xenia/shots/intro_10.png
wait 10
shot private/xenia/shots/intro_20.png
wait 20
shot private/xenia/shots/intro_40.png
wait 20
shot private/xenia/shots/intro_60.png
press START 120
wait 2
shot private/xenia/shots/after_skip_02.png
wait 10
shot private/xenia/shots/after_skip_12.png
```

Run with probes in `private\xenia\boot.txt` (fill the addresses from T1/T2):

```
0x<movie_start> movie_start max=10 r4 lr
0x<movie_surface> movie_surface max=10 r3 r4 lr
0x<movie_row_lookup> movie_row max=10 r4 r5 lr
0x<bgm_play> bgm_play max=40 r4 lr [r4]:u32 [r4+4]:u32 [r4+8]:u32
0x<scene_exit> scene_exit max=20 r4 r5 lr
0x<sound_format> sound_cue max=80 r3 r4 r5 [r3]:u32 [r3+4]:u32 [r3+8]:u32 [r3+12]:u32
```

```powershell
.\tools\xenia\run.ps1 private\xenia\boot.txt -Recipe tools\xenia\recipes\boot_title.txt
```

- A cue pointer read as four `u32` words can be decoded offline as ASCII/UTF-16; use the BGM id and
  `fevdump` event list to confirm the name.
- The passive and input runs answer Q1 (order), Q4 (cues), Q5 (skip) and give the reference PNGs.
- Gate nothing on `env_bgm` here: the front end is exactly the state we want.
- Record the `.jsonl` paths and guest shot paths in the sheets.

### T4. Dump the title assets and text

```powershell
cargo run --release -p acvd-formats --example drbdump -- lang/jp/menu/title.drb.dcx
cargo run --release -p acvd-viewer --bin acvd-menu -- title --lang jp --shot private\tmp\title_jp.png --placeholders
cargo run --release -p acvd-formats --example fmgdump -- lang/en/text/menu/menu.fmg > private\tmp\menu_en.txt
cargo run --release -p acvd-formats --example fmgdump -- lang/jp/text/menu/menu.fmg > private\tmp\menu_jp.txt
```

- Do not dump the full FMG to the chat; use `Select-String` on `private\tmp\menu_*.txt` for the ids
  the DRB references (and around id 204 for the `PRESS START` cross-check).

- Record every dialog/object/texture. Confirm whether `PRESS START` is
  `TextSource::Message { bank: 1, id: 204 }` or a static `RTS` string.
- Record every `TextSource::Runtime` object (it needs game code to fill) and every runtime image slot.
- If `Shape::Other` appears, add its raw record to the notes and decode it against the 360 factory
  (`0x824ac270` for Text, `0x824abaf8` / `0x824ced28` for AlphaAnimSprite, etc.).
- Use `acvd-menu --atlas <texture>` to verify the `title.tpf` texture names/rects.
- Dump the audio candidates:

```powershell
cargo run --release -p acvd-formats --example fevdump -- acv2_bgm_menu_bgm_menu
cargo run --release -p acvd-formats --example fevdump -- acv2_voice_voice
cargo run --release -p acvd-formats --example fevdump -- acv2_se_menu
```

This answers Q3, Q4 and Q6.

### T5. Probe the movie file

Extract the loose movie and inspect it before writing the cache tool:

```powershell
cargo run --release -p acvd-sheets -- dump movie/jp/acv_opening.wmv
ffprobe -v error -show_streams -show_format -of json private\dump\movie_jp_acv_opening.wmv
```

- Confirm the loose path is `movie/jp/acv_opening.wmv`.
- Get fps, frame count, duration, coded size, and audio codec/channels/rate.
- Verify the stream is WMV3 + WMA Pro 6ch as `formats.csv` says.

Write the result to `sheets/movies.csv` (Q7).

## Phase 2 - Sheets

Add/update, every row with 360 evidence or a probe/disc path:

- `sheets/boot_flow.csv` (new): `step,asset,trigger,input,duration,next,evidence`.
  - title appear, blink period, Start press, movie start/end, skip, garage handoff.
- `sheets/movies.csv` (new): `movie,row,file,size,video,audio,width,height,fps,duration,play_type,evidence`.
  - row 0 `acv_opening` first; leave rows 10/11/100 documented but out of scope.
- `sheets/sound_cues.csv`: title BGM cue, title voice cue(s) if any, Start/confirm cue.
- `sheets/ui_anim.csv`: title fades/blinks/title clip lengths, if the DRB has them.
- `sheets/systems.csv`: update the `movies` and `ui` rows (and `sound` if the movie audio path is
  recorded) from todo/in-progress to the actual state.
- `sheets/formats.csv` `.wmv`: record the ASF demux/transcode pipeline once the tool reads the file.

Run `.\target\release\acvd-index.exe audit` after the sheet edits.

## Phase 3 - `tools/acvd-movies`

New workspace member `tools/acvd-movies` (add to `[workspace] members`; do not add to
`default-members` because it needs the generated `acvd-data`).

```
acvd-movies [--disc <360 ISO>] [--out private\movies] [--movie acv_opening] [--fps <source>]
```

1. Resolve the row from `PARAM_MOVIEPLAYINFO_BIN` (`movie_file_name`), then read
   `movie/jp/<name>.wmv` through `vfs::Disc` into `private\movies\src\<name>.wmv`.
2. `ffprobe -v error -select_streams v:0 -show_entries stream=avg_frame_rate,nb_frames,duration,width,height -of json`.
   Fall back to the row's 1280x720 + 30 fps only if ffprobe cannot report.
3. Video: `ffmpeg -i src -an -vf fps=<fps> -q:v 2 out\f%05d.jpg`.
4. Audio: `ffmpeg -i src -vn -ac 2 -ar 48000 -c:a pcm_s16le out\audio.wav`
   (stereo downmix for Bevy; keep the source channel count in `meta.json` and document the downmix).
5. Write `meta.json`: source path, byte length, SHA-256, width/height, fps, frame count, duration,
   audio file/rate/channels, `ffmpeg -version` first line.
6. Re-running with a matching source hash skips. Errors name the missing tool and the exact command.
7. Print a summary (frames, duration, size). Do not commit `private\movies`.
8. Update `README.md` pipeline and `docs/open.md` with the command.

## Phase 4 - Runtime

### R1. Menu text-language override

Add `acvd-render::menu::load_in_lang(disc, layout_path, lang, images)` (or a `text_asset` argument)
so `title.drb` can be loaded from `lang/jp/menu/` while the FMG comes from
`lang/<lang>/text/menu/menu.fmg`. Keep `load` as a wrapper. Add a test that loads the jp title layout
with the en FMG and reads id 204.

### R2. Movie cache player (`acvd-render::video`)

- `MovieCache::open(root, name)` reads `meta.json`, validates the frame count/files and returns
  `Option<MovieCache>` with a one-line reason on failure.
- `MoviePlayer` resource + plugin:
  - full-screen 16:9 node (fit inside the window, black bars when needed; recompute on resize);
  - background thread decodes JPEG frames through a bounded channel (prefetch 3-5 frames) and
    uploads RGBA8 `Image`s; keep a small handle ring so playback never blocks the render thread;
  - frame clock from the `AudioSink` position if available, else wall clock since the audio start;
  - `Start`, `Skip`, `Finished { skipped }` events; stop the sink on skip/finish;
  - volume follows the existing master volume; add a movie/UI bus rather than the positional SFX bus.
- `--boot intro` starts the player immediately for deterministic screenshots; `--wait N --shot`
  must work.

### R3. Title screen (`acvd-game::frontend`)

- `FrontendPlugin` with `BootScreen { Title, Movie, Done }` (or a Bevy `States`).
- On `Title`: spawn a `Camera2d`, set the title clear color, load `title.drb` + `title.tpf` through
  `menu::load_in_lang`, spawn the root dialog at `scale = min(w/1280, h/720)` and center, fill any
  runtime texts/objects, play the traced title BGM/voice cue, and run the DRB `PINA/MINA` fade/blink
  through `ui_kit` (`UiTween`/`UiClip`). Start advances to `Movie`.
- On `Movie`: open the cache; if missing, print the `acvd-movies` command and go to `Done`; otherwise
  start the video + audio. Start (or the traced button) skips to `Done`.
- On `Done`: despawn the front-end camera/UI, restore the normal clear color, set
  `garage::Mode::Garage`, and run the deferred garage setup once.
- Input: match the traced 360 binding exactly (Start). Do not add gameplay keys while the front end
  is active.

### R4. Main integration (`main.rs`)

- Add `--boot <title|intro|garage>` (default `title`), `--skip-boot` (= garage), and `--lang <en|jp|...>`
  (default `en`).
- Existing debug flags (`--garage*`, `--map`, `--layout`, `--test`, `--clip`, `--frame`, `--pause*`,
  `--view-action`, `--sfx`) imply `--boot garage` so current scripts/tests keep working.
- Move `(setup, weapons::setup)` out of `Startup` and run them once on `BootScreen::Done` (or gate
  them with a `world_ready` run condition). Gate the garage/control/HUD update sets on the same
  condition so nothing reads an unspawned AC/map while the title/movie is up.
- Start `SoundPlugin` with map BGM off during the front end; the frontend owns the title cue; on
  `Done`, switch to the garage theme (`grg`, the existing `resolve_bgm_wave(disc, None)`).
- Keep `--shot` working on the title and movie (`--boot title --wait 1 --shot ...`,
  `--boot intro --wait 12.5 --shot ...`).

## Phase 5 - Validation

- Unit tests:
  - jp title layout + en FMG reads id 204;
  - `MovieCache::open` rejects missing/mismatched caches and accepts a synthetic temp cache;
  - `Frontend` transitions Title -> Movie -> Done on Start and on missing cache;
  - `--boot garage` still reaches the garage.
- Visual:
  - `cargo run -p acvd-game -- --boot title --wait 1 --shot private\shots\title.png`
    versus `private/xenia/shots/title_*.png` at 1280x720; check layout, text, logo, blink phase.
  - `cargo run -p acvd-game -- --boot intro --wait 1 --shot private\shots\intro_01.png`,
    `--wait 12.5`, `--wait 60` versus the Xenia `intro_*.png` frames.
- Audio: the cached WAV contains the voice track; verify it starts with the picture and stops on
  skip; the title BGM matches the traced cue.
- Flow: title -> Start -> movie -> garage; skip -> garage; missing cache -> title -> garage with the
  printed command.
- Performance: movie playback holds 30 fps with the decode prefetch; check the `PERF`/`SHOT`
  telemetry.
- `cargo test -p acvd-formats`, `cargo test -p acvd-render`, `cargo test -p acvd-game`,
  `.\target\release\acvd-index.exe audit`.

## Risks / assumptions

- **Order is not assumed**: the passive Xenia capture decides whether `acv_opening` is pre-title,
  post-Start, or an idle attract. If it is not the post-Start movie, stop and confirm with the user
  before changing the scope (the chosen scope is title -> `acv_opening` -> garage).
- **FFmpeg version** changes decoded frames slightly. `meta.json` records the version; the cache is
  per-machine and never committed. The runtime must work without ffmpeg once the cache exists.
- **6-channel audio**: Bevy may not accept 6ch WAV on every device. Start with a stereo downmix,
  record the source channel count, and only preserve 6ch if a quick playback test passes.
- **Unknown DRB shapes**: if `title.drb` uses an undecoded class or a runtime image slot, add the
  decoder/fallback before visual comparison; do not paper over it with a screenshot.
- **Deferring `setup`** touches the main schedule. If the refactor is too invasive, the fallback is
  to keep setup at startup but hide the world, mute map BGM and gate front-end input; that is less
  faithful (map loads before the title) but still playable.
- **JP layout / EN text**: the 360 uses the same layout for every language, but verify English fits
  its text rects; do not silently resize the layout.
- **PINA keyframe values are still undecoded** (`sheets/ui_anim.csv`): the title clip lengths and
  names are readable, but the exact slide/blink pixel values may need inference and visual matching
  against the Xenia shots. Record what was inferred versus traced.
- **Legal boundary**: the frame/WAV cache and the extracted WMV stay in `private/`; never
  `git add -f`, and never commit `private/movies`.

## Completion (per `AGENTS.md`)

- Update `docs/open.md` (move the task to Done), `docs/archive.md` with the one-line summary and the
  360 addresses / probe files / sheet rows, and `sheets/systems.csv` if the system status changed.
- Update `README.md` pipeline with the `acvd-movies` step.
- Stage only the files this task touched (other chats are editing the tree); one commit, then
  `git pull --rebase` and `git push`.
