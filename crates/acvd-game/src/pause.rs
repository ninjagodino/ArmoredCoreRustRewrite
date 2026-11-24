//! Gameplay / AC TEST pause menu built with [`acvd_render::ui_kit::AcvdUiKit`].
//!
//! Press `Esc` (or Gamepad `Start`) during any sortie to freeze gameplay simulation and open the
//! tactical pause overlay:
//! - **RESUME**: closes the pause menu and resumes gameplay.
//! - **OPTIONS**: inline fullscreen toggle and master volume gauge (`settings.json`).
//! - **RETURN TO GARAGE**: shown when in the AC TEST arena (`Scene::is_test_arena()`),
//!   transitioning cleanly back to the 3D Garage with the current AC.

use acvd_render::ui_kit::{
    palette, AcvdUiKit, ButtonState, FontRole, PanelStyle, TraceStyle,
};
use bevy::audio::{AudioSink, AudioSource, GlobalVolume};
use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use bevy::ui::{
    px, AlignItems, FlexDirection, GlobalZIndex, JustifyContent, PositionType, UiRect, Val,
};
use bevy::window::CursorGrabMode;

use crate::garage::{Mode, ModeSwitch, CUE_CANCEL, CUE_CATEGORY, CUE_MOVE, CUE_OK};
use crate::garage_assemble::AssembleUiAssets;
use crate::settings::{self, AudioChannel, GameSettings};
use crate::sound::{self, AudioBus, BgmPlayer, BgmState, Cues};

/// Active page inside the gameplay pause overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PauseScreen {
    #[default]
    Main,
    Options,
}

/// Actionable entries on [`PauseScreen::Main`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseItem {
    Resume,
    Options,
    ReturnToGarage,
}

impl PauseItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Resume => "RESUME",
            Self::Options => "OPTIONS",
            Self::ReturnToGarage => "RETURN TO GARAGE",
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::Resume => "ESC",
            Self::Options => "SETTINGS",
            Self::ReturnToGarage => "AC TEST",
        }
    }
}

/// Returns the ordered main-menu entries for the current sortie context.
/// `RETURN TO GARAGE` is included only when the player is in the test arena.
pub fn main_items(in_test_arena: bool) -> &'static [PauseItem] {
    if in_test_arena {
        &[
            PauseItem::Resume,
            PauseItem::Options,
            PauseItem::ReturnToGarage,
        ]
    } else {
        &[PauseItem::Resume, PauseItem::Options]
    }
}

/// Runtime state of the sortie pause menu.
#[derive(Resource, Default)]
pub struct PauseState {
    pub open: bool,
    pub screen: PauseScreen,
    pub cursor: usize,
    pub dirty: bool,
    pub last_window_size: Vec2,
    pub last_ui_scale: f32,
    pub status_banner: Option<(String, Color)>,
    pub kit: Option<AcvdUiKit>,
}

impl PauseState {
    pub fn open_menu(&mut self, screen: PauseScreen) {
        self.open = true;
        self.screen = screen;
        self.cursor = 0;
        self.status_banner = None;
        self.dirty = true;
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.screen = PauseScreen::Main;
            self.cursor = 0;
            self.status_banner = None;
            self.dirty = true;
        }
    }
}

/// Marker on the root entity of the pause menu UI tree.
#[derive(Component)]
pub struct PauseUi;

#[derive(Component, Clone, Copy)]
pub enum PauseAction {
    MainItem(PauseItem),
    ToggleFullscreen,
    SelectOptionRow(usize),
    StepUiScale(i8),
    StepHudSway(i8),
    StepVolume(AudioChannel, i8),
    BackToMain,
}

/// Bundles Options-menu resources so [`handle_input`] stays within Bevy's SystemParam limit.
#[derive(SystemParam)]
pub struct PauseOptionsCtx<'w, 's> {
    settings: ResMut<'w, GameSettings>,
    global_volume: ResMut<'w, GlobalVolume>,
    bgm: Res<'w, BgmState>,
    sinks: Query<'w, 's, (Entity, &'static mut AudioSink)>,
    bgm_players: Query<'w, 's, Entity, With<BgmPlayer>>,
    audio_buses: Query<'w, 's, &'static AudioBus>,
    windows: Query<'w, 's, &'static mut Window, With<bevy::window::PrimaryWindow>>,
}

/// Bundles audio resources for pause menu cues.
#[derive(SystemParam)]
pub struct PauseAudioCtx<'w> {
    cues: ResMut<'w, Cues>,
    audio_assets: ResMut<'w, Assets<AudioSource>>,
}

const OPTION_CHANNELS: [(usize, AudioChannel); 4] = [
    (3, AudioChannel::Master),
    (4, AudioChannel::Music),
    (5, AudioChannel::Sfx),
    (6, AudioChannel::Ui),
];

fn option_row_count() -> usize {
    8 // 0: Fullscreen, 1: UI Scale, 2: HUD Sway, 3: Master, 4: Music, 5: SFX, 6: Menu/UI, 7: Back
}

fn step_cursor(cursor: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (cursor as isize + delta).rem_euclid(len as isize) as usize
}

fn toggle_fullscreen(
    commands: &mut Commands,
    options: &mut PauseOptionsCtx,
    audio: &mut PauseAudioCtx,
    state: &mut PauseState,
) {
    options.settings.fullscreen = !options.settings.fullscreen;
    if let Ok(mut window) = options.windows.single_mut() {
        settings::apply_fullscreen(&mut window, options.settings.fullscreen);
    }
    options.settings.save();
    sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_OK);
    state.status_banner = Some((
        if options.settings.fullscreen {
            "FULLSCREEN: ON".to_string()
        } else {
            "FULLSCREEN: OFF".to_string()
        },
        palette::ACCENT_TEAL,
    ));
    state.dirty = true;
}

fn step_ui_scale(
    commands: &mut Commands,
    options: &mut PauseOptionsCtx,
    audio: &mut PauseAudioCtx,
    state: &mut PauseState,
    delta: i8,
) {
    options.settings.step_ui_scale(delta);
    options.settings.save();
    sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_MOVE);
    state.status_banner = Some((
        format!("UI SCALE: {}%", options.settings.ui_scale_percent()),
        palette::ACCENT_TEAL,
    ));
    state.dirty = true;
}

fn step_hud_sway(
    commands: &mut Commands,
    options: &mut PauseOptionsCtx,
    audio: &mut PauseAudioCtx,
    state: &mut PauseState,
    delta: i8,
) {
    options.settings.step_hud_sway(delta);
    options.settings.save();
    sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_MOVE);
    state.status_banner = Some((
        format!("HUD SWAY: {}%", options.settings.hud_sway_percent()),
        palette::ACCENT_TEAL,
    ));
    state.dirty = true;
}

fn step_volume(
    commands: &mut Commands,
    options: &mut PauseOptionsCtx,
    audio: &mut PauseAudioCtx,
    state: &mut PauseState,
    channel: AudioChannel,
    delta: i8,
) {
    options.settings.step_channel(channel, delta);
    settings::apply_volume(
        &options.settings,
        &options.bgm,
        &mut audio.cues,
        &mut options.global_volume,
        &mut options.sinks,
        &options.bgm_players,
        &options.audio_buses,
    );
    options.settings.save();
    sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_MOVE);
    state.status_banner = Some((
        format!(
            "{}: {}%",
            channel.label(),
            options.settings.channel_percent(channel)
        ),
        palette::ACCENT_TEAL,
    ));
    state.dirty = true;
}

/// Handles `Esc` / Gamepad `Start` toggle and keyboard/gamepad/mouse navigation while paused.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_input(
    mut commands: Commands,
    mode: Res<Mode>,
    scene: Res<crate::Scene>,
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    gamepads: Query<&Gamepad>,
    mut state: ResMut<PauseState>,
    mut garage_state: ResMut<crate::garage::GarageState>,
    mut mode_switch: ResMut<ModeSwitch>,
    mut options: PauseOptionsCtx,
    mut audio: PauseAudioCtx,
    mut cursor_opts: Query<&mut bevy::window::CursorOptions, With<bevy::window::PrimaryWindow>>,
    btn_q: Query<(Ref<Interaction>, &PauseAction), With<Button>>,
    ui_action_q: Query<(Ref<Interaction>, &crate::garage::UiAction), With<Button>>,
) {
    if *mode != Mode::Sortie {
        if state.open {
            state.close();
        }
        return;
    }

    let gp_start = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::Start));
    let gp_up = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::DPadUp));
    let gp_down = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::DPadDown));
    let gp_left = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::DPadLeft));
    let gp_right = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::DPadRight));
    let gp_ok = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::South));
    let gp_cancel = gamepads
        .iter()
        .any(|g| g.just_pressed(GamepadButton::East));

    let esc = keys.just_pressed(KeyCode::Escape);
    let back = keys.just_pressed(KeyCode::Backspace);

    // Closed -> open on Esc or Gamepad Start.
    if !state.open {
        if esc || gp_start {
            state.open_menu(PauseScreen::Main);
            if let Ok(mut c) = cursor_opts.single_mut() {
                c.grab_mode = CursorGrabMode::None;
                c.visible = true;
            }
            sound::cue(
                &mut commands,
                &mut audio.cues,
                &mut audio.audio_assets,
                CUE_CATEGORY,
            );
        }
        return;
    }

    // Ensure cursor stays unlocked and visible while the pause overlay is active.
    if let Ok(mut c) = cursor_opts.single_mut() {
        if c.grab_mode != CursorGrabMode::None || !c.visible {
            c.grab_mode = CursorGrabMode::None;
            c.visible = true;
        }
    }

    let in_test_arena = scene.is_test_arena();
    let items = main_items(in_test_arena);

    // Mouse button clicks first so clicks feel instant.
    // Defer container row selection (`SelectOptionRow`) so child `<` / `>` arrow buttons win.
    let mut fallback_select_row: Option<usize> = None;
    for (interaction, action) in &btn_q {
        if !interaction.is_changed() || *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            PauseAction::MainItem(item) => {
                activate_main_item(
                    &mut commands,
                    item,
                    &mut state,
                    &mut mode_switch,
                    &mut audio,
                );
                return;
            }
            PauseAction::ToggleFullscreen => {
                state.cursor = 0;
                toggle_fullscreen(&mut commands, &mut options, &mut audio, &mut state);
                return;
            }
            PauseAction::SelectOptionRow(row) => {
                fallback_select_row = Some(row);
                continue;
            }
            PauseAction::StepUiScale(delta) => {
                state.cursor = 1;
                step_ui_scale(&mut commands, &mut options, &mut audio, &mut state, delta);
                return;
            }
            PauseAction::StepHudSway(delta) => {
                state.cursor = 2;
                step_hud_sway(&mut commands, &mut options, &mut audio, &mut state, delta);
                return;
            }
            PauseAction::StepVolume(channel, delta) => {
                if let Some((row, _)) = OPTION_CHANNELS.iter().find(|&&(_, c)| c == channel) {
                    state.cursor = *row;
                }
                step_volume(
                    &mut commands,
                    &mut options,
                    &mut audio,
                    &mut state,
                    channel,
                    delta,
                );
                return;
            }
            PauseAction::BackToMain => {
                state.screen = PauseScreen::Main;
                state.cursor = items
                    .iter()
                    .position(|&i| i == PauseItem::Options)
                    .unwrap_or(0);
                state.status_banner = None;
                state.dirty = true;
                sound::cue(
                    &mut commands,
                    &mut audio.cues,
                    &mut audio.audio_assets,
                    CUE_CANCEL,
                );
                return;
            }
        }
    }
    if let Some(row) = fallback_select_row {
        if state.cursor != row {
            state.cursor = row;
            state.dirty = true;
            sound::cue(
                &mut commands,
                &mut audio.cues,
                &mut audio.audio_assets,
                CUE_MOVE,
            );
        }
        return;
    }

    // Handle clicks and hover-scroll on the shared HSV/RGB Combat HUD Color picker inside Pause Options.
    if state.screen == PauseScreen::Options {
        for (interaction, action) in &ui_action_q {
            if interaction.is_changed() && *interaction == Interaction::Pressed {
                match *action {
                    crate::garage::UiAction::PaintPickerMode(pmode) => {
                        garage_state.paint_picker_mode = pmode;
                        sound::cue(
                            &mut commands,
                            &mut audio.cues,
                            &mut audio.audio_assets,
                            CUE_CATEGORY,
                        );
                        state.dirty = true;
                        return;
                    }
                    crate::garage::UiAction::PaintRgbDelta(comp, delta) => {
                        options.settings.hud_color[comp] =
                            (options.settings.hud_color[comp] as i16 + delta).clamp(0, 255) as u8;
                        options.settings.save();
                        let hc = options.settings.hud_color;
                        garage_state.paint_hsv = crate::paint::rgb_to_hsv([hc[0], hc[1], hc[2], 255]);
                        garage_state.paint_hsv_synced_channel = Some(usize::MAX - 1);
                        sound::cue(
                            &mut commands,
                            &mut audio.cues,
                            &mut audio.audio_assets,
                            CUE_MOVE,
                        );
                        state.dirty = true;
                        return;
                    }
                    _ => {}
                }
            }
        }
        if scroll.delta.y.abs() > 0.0 {
            let delta = crate::garage::wheel_step_direction(scroll.delta.y);
            for (interaction, action) in &ui_action_q {
                if matches!(*interaction, Interaction::Hovered | Interaction::Pressed) {
                    if let crate::garage::UiAction::PaintRgbDelta(comp, _) = *action {
                        options.settings.hud_color[comp] =
                            (options.settings.hud_color[comp] as i16 + delta as i16 * 8)
                                .clamp(0, 255) as u8;
                        options.settings.save();
                        let hc = options.settings.hud_color;
                        garage_state.paint_hsv =
                            crate::paint::rgb_to_hsv([hc[0], hc[1], hc[2], 255]);
                        garage_state.paint_hsv_synced_channel = Some(usize::MAX - 1);
                        sound::cue(
                            &mut commands,
                            &mut audio.cues,
                            &mut audio.audio_assets,
                            CUE_MOVE,
                        );
                        state.dirty = true;
                        return;
                    }
                }
            }
        }
    }

    // Mouse-wheel over hovered Options sliders (UI Scale / audio volume bars) steps their value.
    if state.screen == PauseScreen::Options && scroll.delta.y.abs() > 0.0 {
        let delta = crate::garage::wheel_step_direction(scroll.delta.y);
        for (interaction, action) in &btn_q {
            if !matches!(*interaction, Interaction::Hovered | Interaction::Pressed) {
                continue;
            }
            match *action {
                PauseAction::SelectOptionRow(1) | PauseAction::StepUiScale(_) => {
                    state.cursor = 1;
                    step_ui_scale(&mut commands, &mut options, &mut audio, &mut state, delta);
                    return;
                }
                PauseAction::SelectOptionRow(2) | PauseAction::StepHudSway(_) => {
                    state.cursor = 2;
                    step_hud_sway(&mut commands, &mut options, &mut audio, &mut state, delta);
                    return;
                }
                PauseAction::SelectOptionRow(row) => {
                    if let Some(&(_, channel)) = OPTION_CHANNELS.iter().find(|&&(r, _)| r == row) {
                        state.cursor = row;
                        step_volume(
                            &mut commands,
                            &mut options,
                            &mut audio,
                            &mut state,
                            channel,
                            delta,
                        );
                        return;
                    }
                }
                PauseAction::StepVolume(channel, _) => {
                    if let Some((row, _)) = OPTION_CHANNELS.iter().find(|&&(_, c)| c == channel) {
                        state.cursor = *row;
                    }
                    step_volume(
                        &mut commands,
                        &mut options,
                        &mut audio,
                        &mut state,
                        channel,
                        delta,
                    );
                    return;
                }
                _ => {}
            }
        }
    }

    // Start always closes the pause menu immediately.
    if gp_start {
        state.close();
        sound::cue(
            &mut commands,
            &mut audio.cues,
            &mut audio.audio_assets,
            CUE_CANCEL,
        );
        return;
    }

    // Esc / Backspace / Gamepad B steps back from Options or resumes from Main.
    if esc || back || gp_cancel {
        match state.screen {
            PauseScreen::Main => {
                state.close();
                sound::cue(
                    &mut commands,
                    &mut audio.cues,
                    &mut audio.audio_assets,
                    CUE_CANCEL,
                );
            }
            PauseScreen::Options => {
                state.screen = PauseScreen::Main;
                state.cursor = items
                    .iter()
                    .position(|&i| i == PauseItem::Options)
                    .unwrap_or(0);
                state.status_banner = None;
                state.dirty = true;
                sound::cue(
                    &mut commands,
                    &mut audio.cues,
                    &mut audio.audio_assets,
                    CUE_CANCEL,
                );
            }
        }
        return;
    }

    let up = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) || gp_up;
    let down = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) || gp_down;
    let left = keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) || gp_left;
    let right =
        keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) || gp_right;
    let confirm =
        keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || gp_ok;

    let count = match state.screen {
        PauseScreen::Main => items.len(),
        PauseScreen::Options => option_row_count(),
    };
    if state.cursor >= count {
        state.cursor = count.saturating_sub(1);
    }

    if up || down {
        let delta = if up { -1 } else { 1 };
        state.cursor = step_cursor(state.cursor, count, delta);
        state.dirty = true;
        sound::cue(
            &mut commands,
            &mut audio.cues,
            &mut audio.audio_assets,
            CUE_MOVE,
        );
        return;
    }

    if state.screen == PauseScreen::Options && (left || right) {
        let delta = if right { 1 } else { -1 };
        match state.cursor {
            0 => toggle_fullscreen(&mut commands, &mut options, &mut audio, &mut state),
            1 => step_ui_scale(&mut commands, &mut options, &mut audio, &mut state, delta),
            2 => step_hud_sway(&mut commands, &mut options, &mut audio, &mut state, delta),
            3 => step_volume(
                &mut commands,
                &mut options,
                &mut audio,
                &mut state,
                AudioChannel::Master,
                delta,
            ),
            4 => step_volume(
                &mut commands,
                &mut options,
                &mut audio,
                &mut state,
                AudioChannel::Music,
                delta,
            ),
            5 => step_volume(
                &mut commands,
                &mut options,
                &mut audio,
                &mut state,
                AudioChannel::Sfx,
                delta,
            ),
            6 => step_volume(
                &mut commands,
                &mut options,
                &mut audio,
                &mut state,
                AudioChannel::Ui,
                delta,
            ),
            _ => {}
        }
        return;
    }

    if confirm {
        match state.screen {
            PauseScreen::Main => {
                if let Some(&item) = items.get(state.cursor) {
                    activate_main_item(
                        &mut commands,
                        item,
                        &mut state,
                        &mut mode_switch,
                        &mut audio,
                    );
                }
            }
            PauseScreen::Options => match state.cursor {
                0 => toggle_fullscreen(&mut commands, &mut options, &mut audio, &mut state),
                1 => step_ui_scale(&mut commands, &mut options, &mut audio, &mut state, 1),
                2 => step_hud_sway(&mut commands, &mut options, &mut audio, &mut state, 1),
                3 => step_volume(
                    &mut commands,
                    &mut options,
                    &mut audio,
                    &mut state,
                    AudioChannel::Master,
                    1,
                ),
                4 => step_volume(
                    &mut commands,
                    &mut options,
                    &mut audio,
                    &mut state,
                    AudioChannel::Music,
                    1,
                ),
                5 => step_volume(
                    &mut commands,
                    &mut options,
                    &mut audio,
                    &mut state,
                    AudioChannel::Sfx,
                    1,
                ),
                6 => step_volume(
                    &mut commands,
                    &mut options,
                    &mut audio,
                    &mut state,
                    AudioChannel::Ui,
                    1,
                ),
                7 => {
                    state.screen = PauseScreen::Main;
                    state.cursor = items
                        .iter()
                        .position(|&i| i == PauseItem::Options)
                        .unwrap_or(0);
                    state.status_banner = None;
                    state.dirty = true;
                    sound::cue(
                        &mut commands,
                        &mut audio.cues,
                        &mut audio.audio_assets,
                        CUE_CANCEL,
                    );
                }
                _ => {}
            },
        }
    }
}

fn activate_main_item(
    commands: &mut Commands,
    item: PauseItem,
    state: &mut PauseState,
    mode_switch: &mut ModeSwitch,
    audio: &mut PauseAudioCtx,
) {
    match item {
        PauseItem::Resume => {
            state.close();
            sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_CANCEL);
        }
        PauseItem::Options => {
            state.screen = PauseScreen::Options;
            state.cursor = 0;
            state.status_banner = None;
            state.dirty = true;
            sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_OK);
        }
        PauseItem::ReturnToGarage => {
            state.close();
            mode_switch.target = Some(Mode::Garage);
            sound::cue(commands, &mut audio.cues, &mut audio.audio_assets, CUE_OK);
        }
    }
}

/// Rebuilds the [`AcvdUiKit`] pause overlay whenever [`PauseState::dirty`] or window size changes.
#[allow(clippy::too_many_arguments)]
pub fn sync_ui(
    mut commands: Commands,
    mode: Res<Mode>,
    scene: Res<crate::Scene>,
    garage: Res<crate::Garage>,
    settings: Res<GameSettings>,
    assemble_assets: Res<AssembleUiAssets>,
    mut garage_state: ResMut<crate::garage::GarageState>,
    mut picker_assets: ResMut<crate::garage::PaintPickerAssets>,
    mut images: ResMut<Assets<Image>>,
    mut state: ResMut<PauseState>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    existing: Query<Entity, With<PauseUi>>,
) {
    let win_size = windows
        .single()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));

    if *mode != Mode::Sortie || !state.open {
        for e in &existing {
            commands.entity(e).despawn();
        }
        state.dirty = false;
        return;
    }

    let resized = (win_size - state.last_window_size).length_squared() > 1.0;
    let ui_scale_changed = (settings.ui_scale - state.last_ui_scale).abs() > 1e-4;
    if !state.dirty && !resized && !ui_scale_changed && !existing.is_empty() {
        return;
    }
    state.dirty = false;
    state.last_window_size = win_size;
    state.last_ui_scale = settings.ui_scale;

    for e in &existing {
        commands.entity(e).despawn();
    }

    // Lazily acquire the shared ACVD UI kit (reusing AssembleUiAssets when already loaded).
    if state.kit.is_none() {
        if assemble_assets.initialized {
            state.kit = Some(assemble_assets.kit.clone());
        } else {
            match AcvdUiKit::load(&garage.disc, &mut images) {
                Ok(kit) => state.kit = Some(kit),
                Err(e) => warn!("pause menu: failed to load AcvdUiKit: {e:#}"),
            }
        }
    }
    let fallback_kit = AcvdUiKit::default();
    let kit = state.kit.as_ref().unwrap_or(&fallback_kit);

    let base_scale = (win_size.x / 1280.0).min(win_size.y / 720.0);
    let scale = (base_scale * settings.ui_scale).clamp(0.55, 2.2);
    let in_test_arena = scene.is_test_arena();
    let mode_tag = if in_test_arena {
        "AC TEST ARENA"
    } else {
        "SORTIE"
    };
    let badge_text = if in_test_arena { "AC TEST" } else { "MISSION" };

    // Full-screen translucent tactical scrim above all sortie HUD layers.
    let root = commands
        .spawn((
            PauseUi,
            GlobalZIndex(100),
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.01, 0.03, 0.04, 0.62)),
        ))
        .id();

    let dialog_w = match state.screen {
        PauseScreen::Main => (460.0 * scale).round(),
        PauseScreen::Options => (580.0 * scale).round(),
    };
    let pad = (18.0 * scale).round();
    let gap = (10.0 * scale).round();

    let dialog = kit.spawn_panel(
        &mut commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            width: px(dialog_w),
            padding: UiRect::all(px(pad)),
            flex_direction: FlexDirection::Column,
            row_gap: px(gap),
            ..default()
        },
        scale,
    );

    let title = match state.screen {
        PauseScreen::Main => "PAUSE MENU",
        PauseScreen::Options => "OPTIONS",
    };
    kit.spawn_header(&mut commands, dialog, title, Some(mode_tag), scale);

    // Compact sortie identity strip (AC design name + mode badge).
    let info_bar = kit.spawn_panel(
        &mut commands,
        Some(dialog),
        PanelStyle::Bar,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((10.0 * scale).round()), px((5.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: px((8.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        &mut commands,
        info_bar,
        FontRole::Small,
        &garage.active_name,
        scale,
        palette::TEXT_PRIMARY,
    );
    kit.spawn_badge(
        &mut commands,
        info_bar,
        badge_text,
        palette::ACCENT_TEAL,
        true,
        scale,
    );

    match state.screen {
        PauseScreen::Main => {
            let items = main_items(in_test_arena);
            let list = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: px((7.0 * scale).round()),
                        margin: UiRect::vertical(px((4.0 * scale).round())),
                        ..default()
                    },
                    ChildOf(dialog),
                ))
                .id();

            for (idx, &item) in items.iter().enumerate() {
                let btn_state = if state.cursor == idx {
                    ButtonState::Selected
                } else {
                    ButtonState::Normal
                };
                let btn = kit.spawn_button(
                    &mut commands,
                    list,
                    item.label(),
                    Some(item.tag()),
                    btn_state,
                    scale,
                );
                commands
                    .entity(btn)
                    .insert((Button, PauseAction::MainItem(item)));
            }

            kit.spawn_trace(&mut commands, dialog, TraceStyle::Stepped, scale);

            let footer = kit.spawn_panel(
                &mut commands,
                Some(dialog),
                PanelStyle::Bar,
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((10.0 * scale).round()), px((5.0 * scale).round())),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                scale,
            );
            kit.spawn_text(
                &mut commands,
                footer,
                FontRole::Small,
                "W/S MOVE   ENTER SELECT   ESC RESUME",
                scale,
                palette::TEXT_MUTED,
            );
        }
        PauseScreen::Options => {
            let list = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: px((6.0 * scale).round()),
                        margin: UiRect::vertical(px((2.0 * scale).round())),
                        ..default()
                    },
                    ChildOf(dialog),
                ))
                .id();

            // Section 1: Display & UI Scale
            kit.spawn_text(
                &mut commands,
                list,
                FontRole::Small,
                "DISPLAY",
                scale * 0.92,
                palette::TEXT_MUTED,
            );

            // Row 0: Fullscreen toggle
            let fs_tag = if settings.fullscreen { "ON" } else { "OFF" };
            let fs_state = if state.cursor == 0 {
                ButtonState::Selected
            } else {
                ButtonState::Normal
            };
            let fs_btn = kit.spawn_button(
                &mut commands,
                list,
                "FULLSCREEN",
                Some(fs_tag),
                fs_state,
                scale,
            );
            commands
                .entity(fs_btn)
                .insert((Button, PauseAction::ToggleFullscreen));

            // Row 1: UI Scale slider (75% - 150%, with I marker at 100%)
            let ui_scale_txt = format!("{}%", settings.ui_scale_percent());
            let ui_slider = kit.spawn_slider_row(
                &mut commands,
                list,
                "UI SCALE",
                &ui_scale_txt,
                settings.ui_scale_fill(),
                Some(GameSettings::ui_scale_default_mark()),
                state.cursor == 1,
                132.0,
                scale,
            );
            commands
                .entity(ui_slider.row)
                .insert((Button, PauseAction::SelectOptionRow(1)));
            commands.entity(ui_slider.track).insert(
                crate::garage::SliderTrack(crate::garage::SliderTrackTarget::UiScale),
            );
            commands
                .entity(ui_slider.dec_btn)
                .insert((Button, PauseAction::StepUiScale(-1)));
            commands
                .entity(ui_slider.inc_btn)
                .insert((Button, PauseAction::StepUiScale(1)));

            // Row 2: Crosshair HUD Sway slider (0% - 100%)
            let sway_txt = format!("{}%", settings.hud_sway_percent());
            let sway_slider = kit.spawn_slider_row(
                &mut commands,
                list,
                "HUD SWAY",
                &sway_txt,
                settings.hud_sway.clamp(0.0, 1.0),
                None,
                state.cursor == 2,
                118.0,
                scale,
            );
            commands
                .entity(sway_slider.row)
                .insert((Button, PauseAction::SelectOptionRow(2)));
            commands.entity(sway_slider.track).insert(
                crate::garage::SliderTrack(crate::garage::SliderTrackTarget::HudSway),
            );
            commands
                .entity(sway_slider.dec_btn)
                .insert((Button, PauseAction::StepHudSway(-1)));
            commands
                .entity(sway_slider.inc_btn)
                .insert((Button, PauseAction::StepHudSway(1)));

            // Combat HUD Color HSV / RGB picker
            if garage_state.paint_hsv_synced_channel != Some(usize::MAX - 1) {
                let c = settings.hud_color;
                garage_state.paint_hsv = crate::paint::rgb_to_hsv([c[0], c[1], c[2], 255]);
                garage_state.paint_hsv_synced_channel = Some(usize::MAX - 1);
            }
            picker_assets.ensure_loaded(garage_state.paint_hsv[0], &mut images);
            kit.spawn_text(
                &mut commands,
                list,
                FontRole::Small,
                "COMBAT HUD COLOR",
                scale * 0.86,
                palette::TEXT_MUTED,
            );
            let hc = settings.hud_color;
            crate::garage::spawn_color_picker_card(
                &mut commands,
                list,
                &kit,
                garage_state.paint_picker_mode,
                [hc[0], hc[1], hc[2], 255],
                garage_state.paint_hsv,
                &picker_assets,
                64.0,
                scale * 0.88,
            );

            kit.spawn_trace(&mut commands, list, TraceStyle::Simple, scale);

            // Section 2: Sound Settings (Master, Music, SFX, Menu / UI)
            kit.spawn_text(
                &mut commands,
                list,
                FontRole::Small,
                "SOUND SETTINGS",
                scale * 0.92,
                palette::TEXT_MUTED,
            );

            for &(row_idx, channel) in &OPTION_CHANNELS {
                let pct_txt = format!("{}%", settings.channel_percent(channel));
                let slider = kit.spawn_slider_row(
                    &mut commands,
                    list,
                    channel.short_label(),
                    &pct_txt,
                    settings.channel_volume(channel),
                    None,
                    state.cursor == row_idx,
                    132.0,
                    scale,
                );
                commands
                    .entity(slider.row)
                    .insert((Button, PauseAction::SelectOptionRow(row_idx)));
                commands.entity(slider.track).insert(
                    crate::garage::SliderTrack(crate::garage::SliderTrackTarget::Volume(channel)),
                );
                commands
                    .entity(slider.dec_btn)
                    .insert((Button, PauseAction::StepVolume(channel, -1)));
                commands
                    .entity(slider.inc_btn)
                    .insert((Button, PauseAction::StepVolume(channel, 1)));
            }

            kit.spawn_trace(&mut commands, list, TraceStyle::Simple, scale);

            // Row 6: Back to Pause Main
            let back_state = if state.cursor == 7 {
                ButtonState::Selected
            } else {
                ButtonState::Normal
            };
            let back_btn = kit.spawn_button(
                &mut commands,
                list,
                "BACK",
                Some("ESC"),
                back_state,
                scale,
            );
            commands
                .entity(back_btn)
                .insert((Button, PauseAction::BackToMain));

            if let Some((ref msg, color)) = state.status_banner {
                let banner = kit.spawn_panel(
                    &mut commands,
                    Some(dialog),
                    PanelStyle::Bar,
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(px((10.0 * scale).round()), px((4.0 * scale).round())),
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    scale,
                );
                kit.spawn_text(&mut commands, banner, FontRole::Small, msg, scale, color);
            }

            kit.spawn_trace(&mut commands, dialog, TraceStyle::Stepped, scale);

            let footer = kit.spawn_panel(
                &mut commands,
                Some(dialog),
                PanelStyle::Bar,
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((10.0 * scale).round()), px((5.0 * scale).round())),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                scale,
            );
            kit.spawn_text(
                &mut commands,
                footer,
                FontRole::Small,
                "W/S MOVE   A/D ADJUST   ENTER TOGGLE   ESC BACK",
                scale,
                palette::TEXT_MUTED,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arena_includes_return_to_garage() {
        assert_eq!(
            main_items(true),
            &[
                PauseItem::Resume,
                PauseItem::Options,
                PauseItem::ReturnToGarage,
            ]
        );
    }

    #[test]
    fn normal_gameplay_omits_return_to_garage() {
        assert_eq!(main_items(false), &[PauseItem::Resume, PauseItem::Options]);
    }

    #[test]
    fn cursor_wraps_in_both_directions() {
        assert_eq!(step_cursor(0, 3, -1), 2);
        assert_eq!(step_cursor(2, 3, 1), 0);
        assert_eq!(step_cursor(1, 2, 1), 0);
        assert_eq!(step_cursor(0, 2, -1), 1);
    }
}
