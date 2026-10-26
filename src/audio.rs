//! Fully synthesized game audio: every clip is generated at startup, so the
//! game ships no audio files.
//!
//! Clips are authored as mono f32 PCM at 44.1 kHz, encoded to a minimal
//! PCM16 WAV container ([`wav_bytes`]), and stored in a custom
//! [`SynthClip`] asset. A tiny decoder in this module turns those bytes back
//! into samples for rodio. Using a local asset instead of `bevy::audio::AudioSource`
//! is deliberate: bevy 0.17's default features enable the `vorbis` decoder but
//! not `wav`, and `AudioSource::decoder()` unwraps the rodio decoder, so a
//! WAV-backed `AudioSource` would panic at playback time under default
//! features.
//!
//! Mixing is data-driven from [`GameSettings`] every frame; pause/results
//! transitions only change sink volume, never stop the looped sinks, so
//! resuming is glitch-free.

use std::sync::Arc;
use std::time::Duration;

use bevy::audio::{
    AddAudioSource, AudioPlayer, AudioSink, AudioSinkPlayback, Decodable, PlaybackSettings, Source,
    Volume,
};
use bevy::prelude::*;

use core::tuning::CarId;

use crate::game_state::{
    AppState, GameSettings, ImpactSignal, ScoreRuntime, SelectedCar, UiSoundSignal, VehicleRuntime,
};

/// Sample rate of every synthesized clip (Hz).
const SAMPLE_RATE: u32 = 44_100;
/// Full turn in radians.
const TAU: f32 = std::f32::consts::TAU;

/// Adds synthesized engine, tire, impact, and UI audio.
///
/// Add after `DefaultPlugins`: the plugin registers a clip asset and relies on
/// `AudioPlugin` (part of the default set) for playback output.
pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        // Registers `SynthClip` as an asset and installs its playback systems.
        app.add_audio_source::<SynthClip>()
            .add_systems(Startup, setup_audio)
            .add_systems(
                Update,
                (
                    update_engine_audio,
                    update_squeal_audio,
                    play_impact_sfx,
                    play_ui_sfx,
                    update_countdown_audio,
                ),
            );
    }
}

// ---------------------------------------------------------------------------
// Synthesized asset + decoder
// ---------------------------------------------------------------------------

/// A synthesized clip stored as a PCM16 mono 44.1 kHz WAV file.
///
/// `TypePath` is implemented by hand instead of derived: the workspace
/// dependency named `core` shadows the builtin `core` crate in path
/// resolution, and bevy's `TypePath` derive emits `::core::...` paths. The
/// manual impl keeps the same type-path contract without the derive.
#[derive(Asset, Clone, Debug)]
pub struct SynthClip {
    bytes: Arc<[u8]>,
}

impl bevy::reflect::TypePath for SynthClip {
    fn type_path() -> &'static str {
        "rust_racer::audio::SynthClip"
    }

    fn short_type_path() -> &'static str {
        "SynthClip"
    }

    fn type_ident() -> Option<&'static str> {
        Some("SynthClip")
    }

    fn crate_name() -> Option<&'static str> {
        Some("rust_racer")
    }

    fn module_path() -> Option<&'static str> {
        Some("rust_racer::audio")
    }
}

impl SynthClip {
    /// Encode `samples` (f32, -1..1) into the clip container.
    fn new(samples: &[f32]) -> Self {
        Self {
            bytes: wav_bytes(samples).into(),
        }
    }
}

/// Streams the PCM16 payload of a [`SynthClip`] as f32 samples.
pub struct SynthDecoder {
    bytes: Arc<[u8]>,
    data_start: usize,
    data_len: usize,
    pos: usize,
}

impl SynthDecoder {
    fn new(bytes: Arc<[u8]>) -> Self {
        let (data_start, data_len) = wav_data_chunk(&bytes).unwrap_or((0, 0));
        Self {
            bytes,
            data_start,
            data_len,
            // Sample reads begin at the payload, not at the RIFF header.
            pos: data_start,
        }
    }
}

impl Iterator for SynthDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let end = self.data_start + self.data_len;
        if self.pos + 2 > end {
            return None;
        }
        let raw = u16::from_le_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
        self.pos += 2;
        Some(raw as i16 as f32 / 32768.0)
    }
}

impl Source for SynthDecoder {
    fn current_frame_len(&self) -> Option<usize> {
        // Mono: one sample per frame. Remaining samples of the payload.
        let end = self.data_start + self.data_len;
        Some(end.saturating_sub(self.pos) / 2)
    }

    fn channels(&self) -> u16 {
        1
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(
            (self.data_len / 2) as f64 / SAMPLE_RATE as f64,
        ))
    }
}

impl Decodable for SynthClip {
    type DecoderItem = f32;
    type Decoder = SynthDecoder;

    fn decoder(&self) -> Self::Decoder {
        SynthDecoder::new(self.bytes.clone())
    }
}

/// Locate the RIFF `data` chunk payload in a WAV byte stream.
fn wav_data_chunk(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < 44 {
        return None;
    }
    let mut offset = 12; // skip "RIFF" + size + "WAVE"
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        if id == b"data" {
            let start = offset + 8;
            return Some((start, size.min(bytes.len() - start)));
        }
        // Chunks are word aligned.
        offset += 8 + size + (size & 1);
    }
    None
}

// ---------------------------------------------------------------------------
// Handles and loop markers
// ---------------------------------------------------------------------------

/// Handles to every synthesized clip, inserted once by the startup system.
#[derive(Resource, Clone, Debug)]
pub struct AudioHandles {
    pub engine: Handle<SynthClip>,
    pub squeal: Handle<SynthClip>,
    pub impact: Handle<SynthClip>,
    pub ui_click: Handle<SynthClip>,
    pub countdown: Handle<SynthClip>,
    pub go: Handle<SynthClip>,
    pub bank: Handle<SynthClip>,
}

/// Marks the single looping engine-sound entity.
#[derive(Component)]
struct EngineLoopAudio;

/// Marks a looping tire-squeal entity (rear axle squeal is louder).
#[derive(Component)]
struct SquealLoopAudio {
    rear: bool,
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn setup_audio(mut commands: Commands, mut clips: ResMut<Assets<SynthClip>>) {
    let handles = AudioHandles {
        engine: clips.add(SynthClip::new(&synth_engine())),
        squeal: clips.add(SynthClip::new(&synth_squeal())),
        impact: clips.add(SynthClip::new(&synth_impact())),
        ui_click: clips.add(SynthClip::new(&synth_tone(1200.0, 0.06, 55.0, 0.5))),
        countdown: clips.add(SynthClip::new(&synth_tone(880.0, 0.12, 6.0, 0.5))),
        go: clips.add(SynthClip::new(&synth_tone(1320.0, 0.25, 4.0, 0.6))),
        bank: clips.add(SynthClip::new(&synth_bank())),
    };

    // Both loops start silent; the update systems take over every frame.
    commands.spawn((
        EngineLoopAudio,
        AudioPlayer::<SynthClip>(handles.engine.clone()),
        PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)),
    ));
    for rear in [true, false] {
        commands.spawn((
            SquealLoopAudio { rear },
            AudioPlayer::<SynthClip>(handles.squeal.clone()),
            PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)),
        ));
    }

    commands.insert_resource(handles);
}

// ---------------------------------------------------------------------------
// Per-frame mixing
// ---------------------------------------------------------------------------

fn update_engine_audio(
    time: Res<Time>,
    state: Option<Res<State<AppState>>>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    runtime: Option<Res<VehicleRuntime>>,
    mut sinks: Query<&mut AudioSink, With<EngineLoopAudio>>,
    mut flutter: Local<f32>,
) {
    let app_state = current_state(state.as_deref());
    let (master, engine_gain, _, _) = mix_levels(settings.as_deref());
    let preset = selected
        .as_deref()
        .and_then(|selected| selected.car)
        .unwrap_or(CarId::FairladyZ)
        .preset();
    let redline = preset.redline_rpm.max(1.0);
    let idle = preset.idle_rpm.max(1.0);

    let (rpm, throttle) = match (app_state, runtime.as_deref()) {
        (AppState::Driving | AppState::Paused, Some(runtime)) => (
            runtime.rpm.max(0.0),
            runtime.telemetry.throttle.clamp(0.0, 1.0),
        ),
        _ => (idle, 0.0),
    };
    let rpm_norm = (rpm / redline).clamp(0.0, 1.2);
    let idle_speed = (0.55 + 1.1 * (idle / redline)).clamp(0.4, 2.2);

    let (mut volume, speed) = match app_state {
        AppState::Driving => {
            // Load factor rises with throttle; the base term keeps the engine
            // audible while coasting.
            let load = 0.55 + 0.45 * throttle;
            (
                master * engine_gain * (0.25 + 0.75 * rpm_norm) * load,
                (0.55 + 1.1 * rpm_norm).clamp(0.4, 2.2),
            )
        }
        AppState::Results => (master * engine_gain * 0.18, idle_speed),
        // Pause, loading, and garage: silent but still running.
        _ => (0.0, idle_speed),
    };

    // Idle flutter: ±2% volume wobble, quicker as revs rise.
    *flutter += time.delta_secs() * TAU * (1.5 + 5.0 * rpm_norm);
    volume *= 1.0 + 0.02 * flutter.sin();
    let volume = volume.clamp(0.0, 1.0);

    for mut sink in &mut sinks {
        sink.set_volume(Volume::Linear(volume));
        sink.set_speed(speed);
    }
}

fn update_squeal_audio(
    state: Option<Res<State<AppState>>>,
    settings: Option<Res<GameSettings>>,
    runtime: Option<Res<VehicleRuntime>>,
    mut sinks: Query<(&mut AudioSink, &SquealLoopAudio)>,
) {
    let app_state = current_state(state.as_deref());
    let (master, _, tires, _) = mix_levels(settings.as_deref());
    let driving = app_state == AppState::Driving;

    let (front, rear) = match runtime.as_deref() {
        Some(runtime) if driving => (
            runtime.wheels[0].skid.max(runtime.wheels[1].skid),
            runtime.wheels[2].skid.max(runtime.wheels[3].skid),
        ),
        _ => (0.0, 0.0),
    };

    for (mut sink, marker) in &mut sinks {
        let (skid, gain) = if marker.rear {
            (rear, 1.0)
        } else {
            (front, 0.55)
        };
        let volume = if driving {
            (master * tires * skid.powf(1.3) * 0.9 * gain).clamp(0.0, 1.0)
        } else {
            0.0
        };
        sink.set_volume(Volume::Linear(volume));
        sink.set_speed(0.9 + 0.25 * skid);
    }
}

fn play_impact_sfx(
    mut commands: Commands,
    signal: Option<Res<ImpactSignal>>,
    handles: Option<Res<AudioHandles>>,
    settings: Option<Res<GameSettings>>,
    mut seen: Local<u64>,
) {
    let (Some(signal), Some(handles)) = (signal, handles) else {
        return;
    };
    if signal.seq == *seen {
        return;
    }
    *seen = signal.seq;
    if signal.seq == 0 {
        return;
    }
    let (master, _, _, effects) = mix_levels(settings.as_deref());
    let volume = master * effects * signal.strength.clamp(0.2, 1.0);
    spawn_one_shot(&mut commands, &handles.impact, volume);
}

fn play_ui_sfx(
    mut commands: Commands,
    signal: Option<Res<UiSoundSignal>>,
    handles: Option<Res<AudioHandles>>,
    settings: Option<Res<GameSettings>>,
    mut seen: Local<u64>,
) {
    let (Some(signal), Some(handles)) = (signal, handles) else {
        return;
    };
    if signal.seq == *seen {
        return;
    }
    *seen = signal.seq;
    if signal.seq == 0 {
        return;
    }
    let (master, _, _, effects) = mix_levels(settings.as_deref());
    spawn_one_shot(&mut commands, &handles.ui_click, master * effects * 0.7);
}

fn update_countdown_audio(
    mut commands: Commands,
    time: Res<Time>,
    state: Option<Res<State<AppState>>>,
    score: Option<Res<ScoreRuntime>>,
    handles: Option<Res<AudioHandles>>,
    settings: Option<Res<GameSettings>>,
    mut last_ceil: Local<i32>,
    mut last_banked: Local<f32>,
    mut bank_cooldown: Local<f32>,
) {
    let (Some(score), Some(handles)) = (score, handles) else {
        return;
    };
    let app_state = current_state(state.as_deref());
    let audible = matches!(
        app_state,
        AppState::Driving | AppState::Paused | AppState::Results
    );
    *bank_cooldown = (*bank_cooldown - time.delta_secs()).max(0.0);

    let (master, _, _, effects) = mix_levels(settings.as_deref());
    let gain = master * effects * 0.8;

    // Countdown: one beep as the remaining time crosses each integer second,
    // and the go tone when it reaches zero.
    let ceil = if score.run_active {
        score.run_time_left.ceil() as i32
    } else {
        0
    };
    if !score.run_active {
        *last_ceil = ceil;
    } else if ceil != *last_ceil {
        if audible {
            if (1..=3).contains(&ceil) {
                spawn_one_shot(&mut commands, &handles.countdown, gain);
            } else if ceil <= 0 && *last_ceil > 0 {
                spawn_one_shot(&mut commands, &handles.go, gain);
            }
        }
        *last_ceil = ceil;
    }

    // Combo banks: rising two-tone, rate limited so chained banks never stack.
    let banked = score.score.banked;
    if banked > *last_banked + 0.5 && *bank_cooldown <= 0.0 {
        spawn_one_shot(&mut commands, &handles.bank, gain);
        *bank_cooldown = 0.3;
    }
    *last_banked = banked;
}

fn spawn_one_shot(commands: &mut Commands, clip: &Handle<SynthClip>, volume: f32) {
    commands.spawn((
        AudioPlayer::<SynthClip>(clip.clone()),
        // `Despawn` removes the entity as soon as the sink drains.
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume.clamp(0.0, 1.0))),
    ));
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn current_state(state: Option<&State<AppState>>) -> AppState {
    match state {
        Some(state) => *state.get(),
        // Headless/unit-test setups drive the vehicle directly.
        None => AppState::Driving,
    }
}

/// `(master, engine, tires, effects)` levels, clamped and defaulted.
fn mix_levels(settings: Option<&GameSettings>) -> (f32, f32, f32, f32) {
    if crate::demo::quiet() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    match settings {
        Some(settings) => (
            settings.settings.volume_master.clamp(0.0, 1.0),
            settings.settings.volume_engine.clamp(0.0, 1.0),
            settings.settings.volume_tires.clamp(0.0, 1.0),
            settings.settings.volume_effects.clamp(0.0, 1.0),
        ),
        None => (0.8, 0.7, 0.6, 0.8),
    }
}

// ---------------------------------------------------------------------------
// Synthesis
// ---------------------------------------------------------------------------

/// 0.75 s engine loop: sawtooth fundamental near 55 Hz with two harmonics plus
/// a narrow band of noise, crossfaded at the seam and RMS-normalized to 0.5.
fn synth_engine() -> Vec<f32> {
    let loop_len = (0.75 * SAMPLE_RATE as f32) as usize;
    let fade = (0.05 * SAMPLE_RATE as f32) as usize;
    let total = loop_len + fade;

    // 41 whole cycles per loop: 54.67 Hz (within 1% of the 55 Hz target) keeps
    // the tonal layers periodic so the crossfade only smooths the noise.
    let fundamental = 41.0 / 0.75;
    let step = fundamental / SAMPLE_RATE as f32;
    let narrow = bandpass_noise(total, 1400.0, 3.0, 0x2545_F491);

    let mut phase = 0.0f32;
    let mut raw = Vec::with_capacity(total);
    for &noise in &narrow {
        phase += step;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        let saw = 2.0 * phase - 1.0;
        let second = 2.0 * ((phase * 2.0) % 1.0) - 1.0;
        let third = 2.0 * ((phase * 3.0) % 1.0) - 1.0;
        raw.push(0.55 * saw + 0.26 * second + 0.13 * third + 0.10 * noise);
    }

    let mut out = crossfade_loop(raw, loop_len, fade);
    // Soft saturation tames the crest factor so the 0.5 RMS target fits under
    // the PCM16 ceiling without hard clipping.
    finish_clip(&mut out, 2.0, 0.5);
    out
}

/// 0.6 s tire squeal: noise through a 900 Hz biquad band-pass, RMS 0.4.
fn synth_squeal() -> Vec<f32> {
    let loop_len = (0.6 * SAMPLE_RATE as f32) as usize;
    let fade = (0.05 * SAMPLE_RATE as f32) as usize;
    let raw = bandpass_noise(loop_len + fade, 900.0, 6.0, 0x51ED_2701);
    let mut out = crossfade_loop(raw, loop_len, fade);
    // Heavy saturation of band-passed noise reads as a screech and keeps the
    // 0.4 RMS target under the peak ceiling.
    finish_clip(&mut out, 3.0, 0.4);
    out
}

/// 0.35 s impact: decaying brown-noise burst plus a 60 Hz thump, RMS 0.6.
fn synth_impact() -> Vec<f32> {
    let len = (0.35 * SAMPLE_RATE as f32) as usize;
    let mut rng = 0xDEAD_BEEFu32;
    let mut brown = 0.0f32;
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let t = index as f32 / SAMPLE_RATE as f32;
        let white = hash_noise(&mut rng);
        brown = (brown + 0.02 * white) / 1.02;
        let burst = brown * 1.4 * (-t * 14.0).exp();
        let thump = (TAU * 60.0 * t).sin() * (-t * 9.0).exp() * 0.7;
        out.push(burst + thump);
    }
    apply_edge_fades(&mut out, 0.001);
    // Impacts are transients; saturation is inaudible as distortion but lets
    // the 0.6 RMS burst peak under 1.0.
    finish_clip(&mut out, 6.0, 0.6);
    out
}

/// Short sine tick with an exponential decay; used for beeps and clicks.
fn synth_tone(frequency: f32, duration: f32, decay: f32, peak: f32) -> Vec<f32> {
    let len = (duration * SAMPLE_RATE as f32) as usize;
    let step = frequency / SAMPLE_RATE as f32;
    let mut phase = 0.0f32;
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let t = index as f32 / SAMPLE_RATE as f32;
        phase += step;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        out.push((phase * TAU).sin() * (-t * decay).exp());
    }
    apply_edge_fades(&mut out, 0.006);
    scale_peak(&mut out, peak);
    out
}

/// Rising two-tone for combo banking (E5 then B5) over 0.25 s.
fn synth_bank() -> Vec<f32> {
    let len = (0.25 * SAMPLE_RATE as f32) as usize;
    let split = (0.10 * SAMPLE_RATE as f32) as usize;
    let ramp = (0.005 * SAMPLE_RATE as f32) as usize;
    let mut phase = 0.0f32;
    let mut out = Vec::with_capacity(len);
    for index in 0..len {
        let (frequency, local_t) = if index < split {
            (659.25, index as f32 / SAMPLE_RATE as f32)
        } else {
            (987.77, (index - split) as f32 / SAMPLE_RATE as f32)
        };
        phase += frequency / SAMPLE_RATE as f32;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        out.push((phase * TAU).sin() * (-local_t * 14.0).exp());
    }
    // Ramp the second tone in so the envelope reset is inaudible.
    for index in 0..ramp.min(len.saturating_sub(split)) {
        out[split + index] *= index as f32 / ramp as f32;
    }
    apply_edge_fades(&mut out, 0.005);
    scale_peak(&mut out, 0.45);
    out
}

/// White noise through an RBJ band-pass filter.
fn bandpass_noise(len: usize, frequency: f32, q: f32, seed: u32) -> Vec<f32> {
    let (b0, b1, b2, a1, a2) = biquad_bandpass(frequency, q);
    let mut rng = seed;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let x = hash_noise(&mut rng);
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
        out.push(y);
    }
    out
}

/// RBJ constant-peak-gain band-pass coefficients, normalized by `a0`.
fn biquad_bandpass(frequency: f32, q: f32) -> (f32, f32, f32, f32, f32) {
    let w0 = TAU * frequency / SAMPLE_RATE as f32;
    let alpha = w0.sin() / (2.0 * q.max(0.01));
    let a0 = 1.0 + alpha;
    let b0 = alpha / a0;
    let b1 = 0.0;
    let b2 = -alpha / a0;
    let a1 = -2.0 * w0.cos() / a0;
    let a2 = (1.0 - alpha) / a0;
    (b0, b1, b2, a1, a2)
}

// ---------------------------------------------------------------------------
// Encoding and signal utilities
// ---------------------------------------------------------------------------

/// Minimal PCM16 mono 44.1 kHz WAV encoder.
fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Fold the rendered tail into the head so the clip loops without a click.
fn crossfade_loop(samples: Vec<f32>, loop_len: usize, fade: usize) -> Vec<f32> {
    debug_assert!(samples.len() >= loop_len + fade);
    let mut out = samples[..loop_len].to_vec();
    for index in 0..fade {
        let mix = index as f32 / fade as f32;
        out[index] = out[index] * mix + samples[loop_len + index] * (1.0 - mix);
    }
    out
}

/// Peak-normalize, softly saturate, then set the requested RMS loudness.
///
/// The saturation tames the crest factor so loud clips fit under the PCM16
/// ceiling; without it, peak-limiting would undershoot the RMS targets.
fn finish_clip(samples: &mut [f32], drive: f32, target_rms: f32) {
    scale_peak(samples, 1.0);
    if drive > 0.0 {
        for sample in samples.iter_mut() {
            *sample = (*sample * drive).tanh();
        }
    }
    normalize_rms(samples, target_rms, 0.98);
}

fn normalize_rms(samples: &mut [f32], target_rms: f32, peak_limit: f32) {
    if samples.is_empty() {
        return;
    }
    let sum: f32 = samples.iter().map(|sample| sample * sample).sum();
    let rms = (sum / samples.len() as f32).sqrt();
    if rms <= 1e-6 {
        return;
    }
    let mut gain = target_rms / rms;
    let peak = samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()));
    if peak * gain > peak_limit {
        gain = peak_limit / peak.max(1e-6);
    }
    for sample in samples.iter_mut() {
        *sample *= gain;
    }
}

fn scale_peak(samples: &mut [f32], peak: f32) {
    let current = samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()));
    if current <= 1e-6 {
        return;
    }
    let gain = peak / current;
    for sample in samples.iter_mut() {
        *sample *= gain;
    }
}

/// Linear fades at both ends so one-shots never click on start or stop.
fn apply_edge_fades(samples: &mut [f32], seconds: f32) {
    let fade = ((seconds * SAMPLE_RATE as f32) as usize).min(samples.len() / 2);
    if fade == 0 {
        return;
    }
    for index in 0..fade {
        let ramp = index as f32 / fade as f32;
        samples[index] *= ramp;
        let tail = samples.len() - 1 - index;
        samples[tail] *= ramp;
    }
}

/// Deterministic white noise in -1..1 (no RNG dependency).
fn hash_noise(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let mut x = *state;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x as f32 * (1.0 / 2_147_483_648.0) - 1.0
}
