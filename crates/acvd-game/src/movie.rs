//! Opening-movie playback from the FFmpeg-built cache (`acvd-movies cache`).
//!
//! The rewrite has no ASF/WMV3 decoder, so the cache holds one JPEG per frame plus a stereo
//! 48 kHz PCM `audio.wav` and `manifest.json`. This decodes frames on a worker thread (bounded
//! prefetch) and uses the decoded audio sink's position as the A/V clock, with wall time as the
//! fallback. Everything lives in the gitignored `private/movies/` (retail derivatives).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};

use acvd_render::menu::MenuRoot;
use anyhow::{bail, Context, Result};
use bevy::asset::RenderAssetUsages;
use bevy::audio::{AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, Volume};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::widget::NodeImageMode;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: u32,
    pub duration: f64,
    pub audio: Option<String>,
}

struct Frame {
    idx: u32,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

#[derive(Component)]
pub struct MovieAudio;

#[derive(Resource)]
pub struct Player {
    pub root: Entity,
    pub audio: Option<Entity>,
    pub name: String,
    pub manifest: Manifest,
    rx: Mutex<mpsc::Receiver<Frame>>,
    pending: BTreeMap<u32, Frame>,
    current: Option<u32>,
    image: Option<Handle<Image>>,
    started: f32,
    skip_guard: f32,
    seek: Option<u32>,
    finished: bool,
}

impl Player {
    pub fn finished(&self) -> bool {
        self.finished
    }
}

/// Loads `cache/<name>/manifest.json`, spawns the fullscreen frame node, the decoded audio and
/// the frame worker. `seek` starts paused on one frame (deterministic `--shot` capture) and
/// plays no audio.
pub fn start(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    audio_assets: &mut Assets<AudioSource>,
    cache: &Path,
    name: &str,
    seek: Option<u32>,
) -> Result<Player> {
    let dir = cache.join(name);
    let manifest = read_manifest(&dir, name)?;
    let root = commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            ImageNode::default(),
            BackgroundColor(Color::BLACK),
        ))
        .id();
    let audio = match (&manifest.audio, seek) {
        (Some(file), None) => {
            let bytes = std::fs::read(dir.join(file)).with_context(|| format!("reading {file}"))?;
            let handle = audio_assets.add(AudioSource { bytes: bytes.into() });
            Some(
                commands
                    .spawn((
                        AudioPlayer(handle),
                        PlaybackSettings::ONCE.with_volume(Volume::Linear(0.9)),
                        MovieAudio,
                    ))
                    .id(),
            )
        }
        _ => None,
    };
    let (tx, rx) = mpsc::sync_channel::<Frame>(8);
    let frames_dir = dir.join("frames");
    let first = seek.unwrap_or(0).min(manifest.frames - 1);
    let count = manifest.frames;
    std::thread::spawn(move || {
        for idx in first..count {
            let path = frames_dir.join(format!("{idx:05}.jpg"));
            let Ok(bytes) = std::fs::read(&path) else { break };
            let Ok(decoded) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) else {
                break;
            };
            let rgba = decoded.to_rgba8();
            let (width, height) = rgba.dimensions();
            let frame = Frame { idx, rgba: rgba.into_raw(), width, height };
            if tx.send(frame).is_err() {
                break;
            }
        }
    });
    let mut player = Player {
        root,
        audio,
        name: name.to_string(),
        manifest,
        rx: Mutex::new(rx),
        pending: BTreeMap::new(),
        current: None,
        image: None,
        started: 0.0,
        skip_guard: 0.0,
        seek,
        finished: false,
    };
    if let Some(idx) = seek {
        // Pose the seek immediately so a `--shot` never catches an empty frame.
        while player.pending.len() < 2 {
            match player.rx.get_mut().expect("movie rx").recv_timeout(std::time::Duration::from_millis(5000)) {
                Ok(f) => {
                    player.pending.insert(f.idx, f);
                }
                Err(_) => break,
            }
        }
        let first = idx.min(player.manifest.frames - 1);
        player.show(first, commands, images);
    }
    Ok(player)
}

impl Player {
    fn show(&mut self, idx: u32, commands: &mut Commands, images: &mut Assets<Image>) {
        let Some(frame) = self.pending.remove(&idx) else { return };
        let image = Image::new(
            Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
            TextureDimension::D2,
            frame.rgba,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        let handle = images.add(image);
        if let Some(old) = self.image.replace(handle.clone()) {
            images.remove(&old);
        }
        self.current = Some(idx);
        commands
            .entity(self.root)
            .insert(ImageNode::new(handle).with_mode(NodeImageMode::Stretch));
    }
}

/// Reads and validates `dir/manifest.json`: the cache must exist, describe at least one frame
/// with a positive fps, and hold exactly that many `frames/#####.jpg` files.
fn read_manifest(dir: &Path, name: &str) -> Result<Manifest> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        bail!(
            "{} is not cached; run `cargo run --release -p acvd-movies -- cache {name}`",
            dir.display()
        );
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    if manifest.frames == 0 || manifest.fps <= 0.0 {
        bail!("{} has no frames", manifest_path.display());
    }
    let on_disk = std::fs::read_dir(dir.join("frames")).map_or(0, |it| {
        it.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jpg"))
            .count()
    });
    if on_disk != manifest.frames as usize {
        bail!(
            "{} is incomplete: {on_disk} frames on disk, manifest says {}; rerun `acvd-movies cache {name} --force`",
            dir.display(),
            manifest.frames
        );
    }
    Ok(manifest)
}

/// Advances the player, setting [`Player::finished`] at the end (or on skip).
pub fn update(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    sinks: Query<&AudioSink, With<MovieAudio>>,
    mut player: Option<ResMut<Player>>,
) {
    let Some(player) = player.as_deref_mut() else { return };
    if player.finished {
        return;
    }
    if player.started == 0.0 {
        player.started = time.elapsed_secs();
    }
    player.skip_guard += time.delta_secs();
    while let Ok(frame) = player.rx.get_mut().expect("movie rx").try_recv() {
        player.pending.insert(frame.idx, frame);
    }
    if player.pending.len() > 20 {
        let keep_from = player.current.unwrap_or(0).saturating_sub(2);
        player.pending.retain(|&i, _| i >= keep_from);
    }
    let target = match player.seek {
        Some(idx) => idx.min(player.manifest.frames - 1),
        None => {
            let elapsed = time.elapsed_secs() - player.started;
            let position = sinks.iter().next().map(|s| s.position().as_secs_f32());
            let t = position.unwrap_or(elapsed);
            (t * player.manifest.fps as f32).floor() as u32
        }
    };
    if player.current != Some(target) && player.pending.contains_key(&target) {
        player.show(target, &mut commands, &mut images);
    }
    let skip = player.skip_guard > 0.6
        && (keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space)
            || keys.just_pressed(KeyCode::Escape)
            || keys.just_pressed(KeyCode::NumpadEnter));
    let ended = player.seek.is_none()
        && time.elapsed_secs() - player.started >= player.manifest.duration as f32;
    if skip || ended {
        player.finished = true;
    }
}

pub fn despawn(commands: &mut Commands, player: &Player) {
    commands.entity(player.root).despawn();
    if let Some(audio) = player.audio {
        commands.entity(audio).despawn();
    }
}

/// Names of the movies present in a cache directory (for the boot fallback message).
pub fn cached_movies(cache: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().join("manifest.json").is_file())
        .map(|e| e.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        .filter(|n| !n.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Marker for the frontend's own 2D camera so the boot flow can tear it down.
#[derive(Component)]
pub struct FrontendCamera;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acvd-movie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        dir
    }

    fn write_manifest(dir: &Path, frames: u32) {
        let json = format!(
            r#"{{"name":"test","source":"movie/jp/test.wmv","width":1280,"height":720,"fps":29.97,"frames":{frames},"duration":1.0,"audio":null}}"#
        );
        std::fs::write(dir.join("manifest.json"), json).unwrap();
    }

    #[test]
    fn manifest_rejects_missing_and_incomplete_caches() {
        let dir = temp_cache("reject");
        assert!(read_manifest(&dir, "test").is_err(), "missing manifest");
        write_manifest(&dir, 2);
        assert!(read_manifest(&dir, "test").is_err(), "0 of 2 frames");
        std::fs::write(dir.join("frames/00000.jpg"), b"x").unwrap();
        std::fs::write(dir.join("frames/00001.jpg"), b"x").unwrap();
        let m = read_manifest(&dir, "test").unwrap();
        assert_eq!((m.frames, m.width, m.height), (2, 1280, 720));
        std::fs::remove_file(dir.join("frames/00001.jpg")).unwrap();
        assert!(read_manifest(&dir, "test").is_err(), "1 of 2 frames");
        write_manifest(&dir, 0);
        assert!(read_manifest(&dir, "test").is_err(), "0 frames");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
