//! COM voice subtitles: the retail mission text window that appears while the CPU voice speaks.
//!
//! The 360 mission frontend binds the `MissionText` dialog (`0x825ad978` -> `0x82519228`,
//! `MsgBack`/`MsgText`/`MsgNameText`/`MsgNameBack`) and shows it for a caller-supplied duration
//! (`0x825196a8` writes the timer at window +0x170, `0x82518d18` counts it down and hides the
//! nodes through `0x82b800a8` bit 6). `lang/en/menu/sortie.drb.dcx` lays the window out at
//! x 139..1142, y 559..629 with the name plate at x 267..603, y 519..557; the message line is
//! centred in x 240..1040, y 570..617 (dialog 44 `MissionText`, `sheets/ui_anim.csv`).
//!
//! The rewrite draws that geometry with [`AcvdUiKit`] so it follows `ui_scale`; text comes from
//! `lang/en/text/ai/aisys.fmg` through the `AI_MESSAGE_SOUND_MAP` text id (`voice::text_id`).

use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::Disc;
use acvd_render::ui_kit::{palette, AcvdUiKit, FontRole, PanelStyle, UiClip};
use bevy::prelude::*;
use bevy::ui::px;
use bevy::window::PrimaryWindow;

use crate::control::Piloting;
use crate::garage_assemble::AssembleUiAssets;
use crate::settings::{GameSettings, UI_SCALE_MAX, UI_SCALE_MIN};
use crate::voice::{text_id, ComLine};
use crate::{Garage, ShowHud};

const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);
/// `MissionText` MsgBack (`LineText_base`, dialog 212) placed at [0, 556, 1280, 633] with the
/// 70 px middle cap; the visible bar is 139..1142 x 559..629.
const LINE_RECT: [f32; 4] = [139.0, 559.0, 1142.0, 629.0];
/// `MissionText` MsgNameBack -> `LineText_base＿2` (dialog 257).
const NAME_RECT: [f32; 4] = [267.0, 519.0, 603.0, 557.0];
/// `MissionText` MsgText text rect (font 17, colour 0xA0A082FF, centre/centre).
const TEXT_RECT: [f32; 4] = [240.0, 570.0, 1040.0, 617.0];
const SPEAKER: &str = "COM";
const FALLBACK_SECONDS: f32 = 4.0;

/// English AI subtitle bank (`lang/en/text/ai/aisys.fmg` 9001..9044), loaded once on the first
/// spoken line. `text_id` resolves the offset through the generated message table.
#[derive(Resource, Default)]
pub struct ComSubtitleBank {
    fmg: Option<Fmg>,
    attempted: bool,
}

impl ComSubtitleBank {
    fn ensure(&mut self, disc: &Disc) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        match disc
            .asset("lang/en/text/ai/aisys.fmg")
            .and_then(|bytes| acvd_formats::fmg::read(&bytes))
        {
            Ok(fmg) => self.fmg = Some(fmg),
            Err(err) => warn!("COM subtitles: aisys.fmg: {err:#}"),
        }
    }

    /// Subtitle text for one `AI_MESSAGE_SOUND_MAP` voice offset, normalised to the UI fonts.
    pub fn line(&self, offset: u32) -> Option<String> {
        let id = text_id(offset)?;
        let text = self.fmg.as_ref()?.get(id)?;
        Some(normalize(text))
    }
}

/// The retail window uses the full-width CJK percent sign; the menu CCM fonts carry ASCII `%`.
fn normalize(text: &str) -> String {
    text.replace('\u{ff05}', "%")
        .replace('\r', "")
        .replace('\0', "")
        .trim()
        .to_string()
}

/// How long the subtitle stays up: the decoded voice-clip length plus a short tail, clamped so a
/// missing or unusually long clip still behaves. Retail's timer is caller-supplied.
fn display_seconds(seconds: Option<f32>) -> f32 {
    seconds
        .map(|s| (s + 0.8).clamp(2.5, 9.0))
        .unwrap_or(FALLBACK_SECONDS)
}

struct ActiveSubtitle {
    text: String,
    remaining: f32,
    root: Option<Entity>,
    fading: bool,
}

#[derive(Resource, Default)]
struct ComSubtitle {
    kit: Option<AcvdUiKit>,
    kit_attempted: bool,
    active: Option<ActiveSubtitle>,
    built_for: Option<(Vec2, f32)>,
}

/// Marker for the subtitle's full-screen layout root.
#[derive(Component)]
struct ComSubtitleRoot;

/// Shows the retail mission text window when a COM voice line starts.
pub struct ComSubtitlePlugin;

impl Plugin for ComSubtitlePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ComSubtitleBank>()
            .init_resource::<ComSubtitle>()
            .add_systems(
                Update,
                (clear_outside_sortie, receive_lines, rebuild, tick)
                    .chain()
                    .after(crate::voice::trigger_messages),
            );
    }
}

fn subtitle_visible(piloting: &Piloting, show_hud: Option<&ShowHud>) -> bool {
    piloting.0 && show_hud.map_or(true, |show| show.0)
}

fn clear_outside_sortie(
    mut commands: Commands,
    piloting: Res<Piloting>,
    show_hud: Option<Res<ShowHud>>,
    mut state: ResMut<ComSubtitle>,
) {
    if subtitle_visible(&piloting, show_hud.as_deref()) {
        return;
    }
    if let Some(active) = state.active.take() {
        if let Some(root) = active.root {
            commands.entity(root).despawn();
        }
    }
    state.built_for = None;
}

fn receive_lines(
    mut commands: Commands,
    mut lines: MessageReader<ComLine>,
    garage: Res<Garage>,
    piloting: Res<Piloting>,
    show_hud: Option<Res<ShowHud>>,
    mut bank: ResMut<ComSubtitleBank>,
    mut state: ResMut<ComSubtitle>,
) {
    if !subtitle_visible(&piloting, show_hud.as_deref()) {
        lines.clear();
        return;
    }
    let Some(line) = lines.read().last().copied() else {
        return;
    };
    bank.ensure(&garage.disc);
    let Some(text) = bank.line(line.offset) else {
        return;
    };
    if let Some(old) = state.active.take() {
        if let Some(root) = old.root {
            commands.entity(root).despawn();
        }
    }
    state.built_for = None;
    state.active = Some(ActiveSubtitle {
        text,
        remaining: display_seconds(line.seconds),
        root: None,
        fading: false,
    });
}

fn rebuild(
    mut commands: Commands,
    garage: Res<Garage>,
    assemble: Res<AssembleUiAssets>,
    settings: Res<GameSettings>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut state: ResMut<ComSubtitle>,
) {
    let ComSubtitle {
        kit: kit_slot,
        kit_attempted,
        active,
        built_for,
    } = &mut *state;
    let Some(active) = active.as_mut() else {
        return;
    };
    let Ok(window) = window.single() else {
        return;
    };
    let size = window.size();
    if size.min_element() <= 0.0 {
        return;
    }
    let ui_scale = settings.ui_scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX);
    let layout = (size, ui_scale);
    if built_for.is_some_and(|built| built != layout) {
        if let Some(root) = active.root.take() {
            commands.entity(root).despawn();
        }
        *built_for = None;
    }
    if active.root.is_some() {
        return;
    }

    if kit_slot.is_none() && !*kit_attempted {
        *kit_attempted = true;
        if assemble.initialized {
            *kit_slot = Some(assemble.kit.clone());
        } else {
            match AcvdUiKit::load(&garage.disc, &mut images) {
                Ok(kit) => *kit_slot = Some(kit),
                Err(err) => warn!("COM subtitles: failed to load AcvdUiKit: {err:#}"),
            }
        }
    }
    let kit = kit_slot.clone().unwrap_or_default();

    let base_scale = (size / SCREEN).min_element();
    let scale = base_scale * ui_scale;
    let corner = (size - SCREEN * scale) / 2.0;
    let text = active.text.clone();
    let root = spawn_window(&mut commands, &kit, corner, scale, &text);
    active.root = Some(root);
    *built_for = Some(layout);
}

fn spawn_window(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    corner: Vec2,
    scale: f32,
    text: &str,
) -> Entity {
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(corner.x),
                top: px(corner.y),
                width: px(SCREEN.x * scale),
                height: px(SCREEN.y * scale),
                ..Default::default()
            },
            ComSubtitleRoot,
        ))
        .id();

    kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        rect_node(LINE_RECT, scale),
        scale,
    );
    let name = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Bar,
        Node {
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..rect_node(NAME_RECT, scale)
        },
        scale,
    );
    kit.spawn_text(
        commands,
        name,
        FontRole::Header,
        SPEAKER,
        scale * 0.95,
        palette::TEXT_BRIGHT,
    );
    let text_box = commands
        .spawn((
            Node {
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..rect_node(TEXT_RECT, scale)
            },
            ChildOf(root),
        ))
        .id();
    kit.spawn_text(
        commands,
        text_box,
        FontRole::Body,
        text,
        scale,
        palette::TEXT_PRIMARY,
    );

    kit.attach_tween(commands, root, UiClip::FadeIn5f.tween(0));
    root
}

fn rect_node(rect: [f32; 4], scale: f32) -> Node {
    let [left, top, right, bottom] = rect;
    Node {
        position_type: PositionType::Absolute,
        left: px(left * scale),
        top: px(top * scale),
        width: px((right - left) * scale),
        height: px((bottom - top) * scale),
        ..Default::default()
    }
}

fn tick(mut commands: Commands, time: Res<Time>, mut state: ResMut<ComSubtitle>) {
    let ComSubtitle {
        active: active_slot,
        built_for,
        ..
    } = &mut *state;
    let Some(active) = active_slot.as_mut() else {
        return;
    };
    let Some(root) = active.root else {
        return;
    };
    active.remaining -= time.delta_secs();
    if active.remaining <= 0.0 {
        commands.entity(root).despawn();
        *active_slot = None;
        *built_for = None;
    } else if active.remaining <= 0.30 && !active.fading {
        active.fading = true;
        let tween = UiClip::FrameExit.tween(0);
        commands
            .entity(root)
            .insert((tween.clone(), tween.initial_transform()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_ids_follow_the_ai_message_table() {
        assert_eq!(text_id(1), Some(9018));
        assert_eq!(text_id(2), Some(9019));
        assert_eq!(text_id(3), Some(9020));
        assert_eq!(text_id(14), Some(9026));
        assert_eq!(text_id(22), Some(9014));
        assert_eq!(text_id(24), Some(9001));
        assert_eq!(text_id(31), Some(9012));
        assert_eq!(text_id(34), Some(9034));
        assert_eq!(text_id(44), Some(9044));
        assert_eq!(text_id(0), None);
        assert_eq!(text_id(45), None);
    }

    #[test]
    fn full_width_percent_is_normalised() {
        assert_eq!(normalize("AP at 50％."), "AP at 50%.");
        assert_eq!(normalize("System: Scan Mode\0"), "System: Scan Mode");
    }

    #[test]
    fn subtitle_timer_uses_the_voice_clip_with_clamps() {
        assert_eq!(display_seconds(None), FALLBACK_SECONDS);
        assert!((display_seconds(Some(1.0)) - 2.5).abs() < 1e-6);
        assert!((display_seconds(Some(2.2)) - 3.0).abs() < 1e-6);
        assert!((display_seconds(Some(30.0)) - 9.0).abs() < 1e-6);
    }

    #[test]
    fn aisys_bank_resolves_the_spoken_lines() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&acvd_formats::vfs::default_disc(&root)) else {
            return;
        };
        let mut bank = ComSubtitleBank::default();
        bank.ensure(&disc);
        assert_eq!(bank.line(1).as_deref(), Some("AP at 50%."));
        assert_eq!(bank.line(24).as_deref(), Some("Target sighted. Engaging."));
        assert_eq!(bank.line(19).as_deref(), Some("Beginning operation."));
        assert_eq!(
            bank.line(44).as_deref(),
            Some("Test OK. Continuing operation.")
        );
    }
}
