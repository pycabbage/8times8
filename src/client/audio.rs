use std::{
    f32::consts::TAU,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

use cpal::{
    FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

const COMMAND_LIMIT: usize = 64;
const NOTE_LIMIT: usize = 48;
const WAVE_LIMIT: usize = 32;
const WAVE_TICKS: usize = 4;
const WAVE_DURATION: Duration = Duration::from_millis(600);
const GAIN_FLOOR: f32 = 0.0001;

pub(super) struct Audio {
    enabled: bool,
    attempted: bool,
    output: Option<Output>,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            enabled: true,
            attempted: false,
            output: None,
        }
    }
}

impl Audio {
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if let Some(output) = &self.output {
            // Each toggle invalidates existing notes, echoes and queued messages, even
            // if both off and on occur between two audio callbacks.
            output.generation.fetch_add(1, Ordering::Release);
        } else {
            self.attempted = false;
        }
    }

    pub(super) fn flip(&mut self) {
        self.flip_with_volume(1.0);
    }

    pub(super) fn flip_with_volume(&mut self, volume: f32) {
        if volume.is_finite() && volume > 0.0 {
            self.submit(Command::Flip(volume.min(1.0)));
        }
    }

    pub(super) fn wave(&mut self, duration: Duration, ticks: usize) {
        if ticks != 0 {
            self.submit(Command::Wave(
                duration.min(WAVE_DURATION),
                ticks.min(WAVE_TICKS),
            ));
        }
    }

    pub(super) fn completed(&mut self) {
        self.submit(Command::Completed);
    }

    pub(super) fn item(&mut self) {
        self.submit(Command::Item);
    }

    fn submit(&mut self, command: Command) {
        if !self.enabled {
            return;
        }
        // Headless or unavailable outputs simply stay silent. Avoid trying to open the
        // device again for every tile; toggling sound allows a later retry.
        if !self.attempted {
            self.attempted = true;
            self.output = Output::open();
        }
        if let Some(output) = &self.output {
            let _ = output.inbox.try_send(Queued {
                generation: output.generation.load(Ordering::Relaxed),
                command,
            });
        }
    }
}

struct Output {
    inbox: SyncSender<Queued>,
    generation: Arc<AtomicUsize>,
    _stream: cpal::Stream,
}

impl Output {
    fn open() -> Option<Self> {
        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let format = supported.sample_format();
        let config = supported.config();
        if config.channels == 0 || !(8_000..=384_000).contains(&config.sample_rate) {
            return None;
        }

        let (inbox, receiver) = mpsc::sync_channel(COMMAND_LIMIT);
        let generation = Arc::new(AtomicUsize::new(0));
        let callback = Callback {
            synth: Synth::new(config.sample_rate),
            receiver,
            generation: generation.clone(),
            current: 0,
        };
        let stream = match format {
            SampleFormat::F32 => build_stream::<f32>(&device, config, callback),
            SampleFormat::F64 => build_stream::<f64>(&device, config, callback),
            SampleFormat::I8 => build_stream::<i8>(&device, config, callback),
            SampleFormat::I16 => build_stream::<i16>(&device, config, callback),
            SampleFormat::I24 => build_stream::<cpal::I24>(&device, config, callback),
            SampleFormat::I32 => build_stream::<i32>(&device, config, callback),
            SampleFormat::I64 => build_stream::<i64>(&device, config, callback),
            SampleFormat::U8 => build_stream::<u8>(&device, config, callback),
            SampleFormat::U16 => build_stream::<u16>(&device, config, callback),
            SampleFormat::U24 => build_stream::<cpal::U24>(&device, config, callback),
            SampleFormat::U32 => build_stream::<u32>(&device, config, callback),
            SampleFormat::U64 => build_stream::<u64>(&device, config, callback),
            _ => return None,
        }
        .ok()?;
        stream.play().ok()?;
        Some(Self {
            inbox,
            generation,
            _stream: stream,
        })
    }
}

fn build_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut callback: Callback,
) -> Result<cpal::Stream, cpal::Error> {
    let channels = usize::from(config.channels);
    device.build_output_stream(
        config,
        move |buffer: &mut [T], _| callback.render(buffer, channels),
        |_| {},
        Some(Duration::from_millis(250)),
    )
}

struct Queued {
    generation: usize,
    command: Command,
}

enum Command {
    Flip(f32),
    Wave(Duration, usize),
    Completed,
    Item,
}

struct Callback {
    synth: Synth,
    receiver: Receiver<Queued>,
    generation: Arc<AtomicUsize>,
    current: usize,
}

impl Callback {
    fn sync_generation(&mut self) {
        let generation = self.generation.load(Ordering::Acquire);
        if generation != self.current {
            self.current = generation;
            self.synth.clear();
        }
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, buffer: &mut [T], channels: usize) {
        self.sync_generation();
        for _ in 0..COMMAND_LIMIT {
            let Ok(queued) = self.receiver.try_recv() else {
                break;
            };
            if queued.generation != self.current {
                self.sync_generation();
            }
            if queued.generation == self.current && self.current.is_multiple_of(2) {
                self.synth.handle(queued.command);
            }
        }
        self.sync_generation();
        if self.current.is_multiple_of(2) {
            for frame in buffer.chunks_mut(channels) {
                frame.fill(T::from_sample(self.synth.sample()));
            }
        } else {
            buffer.fill(T::from_sample(0.0));
        }
    }
}

const CHIME_PITCHES: [f32; 4] = [523.25, 783.99, 1046.5, 1318.51];
const ITEM_PITCHES: [f32; 5] = [659.25, 783.99, 987.77, 1174.66, 1567.98];

struct Synth {
    rate: u32,
    frame: u64,
    rng: u32,
    flips: Density,
    chimes: Density,
    next_chime: usize,
    notes: Vec<Voice>,
    ticks: [Option<Tick>; WAVE_LIMIT],
    next_tick: Option<u64>,
    echo: Echo,
    compressor: Compressor,
}

impl Synth {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            frame: 0,
            rng: 0x6b8b_4567,
            flips: Density::default(),
            chimes: Density::default(),
            next_chime: 0,
            // Capacity is reserved off the audio thread and never grows in the callback.
            notes: Vec::with_capacity(NOTE_LIMIT),
            ticks: [None; WAVE_LIMIT],
            next_tick: None,
            echo: Echo::new(rate),
            compressor: Compressor::new(rate),
        }
    }

    fn clear(&mut self) {
        self.flips = Density::default();
        self.chimes = Density::default();
        self.next_chime = 0;
        self.notes.clear();
        self.ticks.fill(None);
        self.next_tick = None;
        self.echo.clear();
        self.compressor.gain = 1.0;
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Flip(volume) => self.flip(volume),
            Command::Wave(duration, ticks) => self.wave(duration, ticks),
            Command::Completed => self.completed(),
            Command::Item => self.item(),
        }
    }

    fn note(&mut self, start_ms: u32, tone: Tone) {
        if self.notes.len() < NOTE_LIMIT {
            let start = self.frame + u64::from(sample_count(self.rate, start_ms));
            self.notes.push(Voice::new(self.rate, start, tone));
        }
    }

    fn flip(&mut self, volume: f32) {
        let Some(energy) = self.flips.accept(self.frame, self.rate, 28, 0.35) else {
            return;
        };
        // Tiny pitch variations keep a long run from sounding like a single tone.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        let spread = self.rng as f32 / u32::MAX as f32 - 0.5;
        self.note(
            0,
            Tone {
                pitch: 640.0 * (spread * 0.16).exp2(),
                peak: 0.11 * volume / (1.0 + (energy - 1.0) * 0.35),
                attack_ms: 5,
                decay_ms: 130,
                stop_ms: 150,
                kind: Kind::Flip,
            },
        );
    }

    fn wave(&mut self, duration: Duration, ticks: usize) {
        if ticks == 0 {
            return;
        }
        let interval = duration.as_secs_f64() * f64::from(self.rate) / ticks as f64;
        let mut volume = 0.6;
        for k in 1..=ticks {
            let Some(slot) = self.ticks.iter_mut().find(|slot| slot.is_none()) else {
                break;
            };
            let frame = self.frame + (interval * k as f64).round() as u64;
            *slot = Some(Tick { frame, volume });
            self.next_tick = Some(self.next_tick.map_or(frame, |next| next.min(frame)));
            volume *= 0.72;
        }
    }

    fn completed(&mut self) {
        let Some(energy) = self.chimes.accept(self.frame, self.rate, 120, 2.0) else {
            return;
        };
        // Let an isolated completion ring out. During bursts play only one step of
        // the chord, getting quieter as the frequency of completions rises.
        if energy > 1.35 {
            let pitch = CHIME_PITCHES[self.next_chime];
            self.next_chime = (self.next_chime + 1) % CHIME_PITCHES.len();
            self.note(
                60,
                Tone {
                    pitch,
                    peak: 0.032 / energy.sqrt(),
                    attack_ms: 20,
                    decay_ms: 1_300,
                    stop_ms: 1_400,
                    kind: Kind::Dry,
                },
            );
        } else {
            self.next_chime = 0;
            for (k, pitch) in CHIME_PITCHES.into_iter().enumerate() {
                self.note(
                    80 + k as u32 * 110,
                    Tone {
                        pitch,
                        peak: 0.05 - k as f32 * 0.007,
                        attack_ms: 30,
                        decay_ms: 2_600,
                        stop_ms: 2_700,
                        kind: Kind::Echoed,
                    },
                );
            }
        }
    }

    fn item(&mut self) {
        for (k, pitch) in ITEM_PITCHES.into_iter().enumerate() {
            self.note(
                50 + k as u32 * 90,
                Tone {
                    pitch,
                    peak: 0.038 - k as f32 * 0.004,
                    attack_ms: 20,
                    decay_ms: 1_900,
                    stop_ms: 2_000,
                    kind: Kind::Echoed,
                },
            );
        }
    }

    fn sample(&mut self) -> f32 {
        if self.next_tick.is_some_and(|frame| frame <= self.frame) {
            self.next_tick = None;
            for k in 0..self.ticks.len() {
                if let Some(tick) = self.ticks[k] {
                    if tick.frame <= self.frame {
                        self.ticks[k] = None;
                        self.flip(tick.volume);
                    } else {
                        self.next_tick =
                            Some(self.next_tick.map_or(tick.frame, |n| n.min(tick.frame)));
                    }
                }
            }
        }

        let mut dry = 0.0;
        let mut wet = 0.0;
        let mut k = 0;
        while k < self.notes.len() {
            let note = &mut self.notes[k];
            if let Some(signal) = note.sample(self.frame) {
                dry += signal;
                if note.kind == Kind::Echoed {
                    wet += signal;
                }
                k += 1;
            } else {
                self.notes.swap_remove(k);
            }
        }
        self.frame += 1;
        self.compressor
            .process((dry + self.echo.process(wet)) * 0.9)
    }
}

fn sample_count(rate: u32, millis: u32) -> u32 {
    ((u64::from(rate) * u64::from(millis) + 500) / 1_000) as u32
}

#[derive(Default)]
struct Density {
    last: Option<u64>,
    energy: f32,
}

impl Density {
    fn accept(&mut self, frame: u64, rate: u32, gap_ms: u32, decay_secs: f32) -> Option<f32> {
        if let Some(last) = self.last {
            let elapsed = frame - last;
            if elapsed < u64::from(sample_count(rate, gap_ms)) {
                return None;
            }
            self.energy *= (-(elapsed as f32) / rate as f32 / decay_secs).exp();
        }
        self.energy += 1.0;
        self.last = Some(frame);
        Some(self.energy)
    }
}

#[derive(Clone, Copy)]
struct Tick {
    frame: u64,
    volume: f32,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Flip,
    Dry,
    Echoed,
}

struct Tone {
    pitch: f32,
    peak: f32,
    attack_ms: u32,
    decay_ms: u32,
    stop_ms: u32,
    kind: Kind,
}

struct Voice {
    start: u64,
    end: u64,
    attack: u32,
    decay: u32,
    peak: f32,
    gain: f32,
    rise: f32,
    fall: f32,
    phase: f32,
    step: f32,
    kind: Kind,
    sweep: Option<Sweep>,
}

struct Sweep {
    end: u32,
    target: f32,
    multiplier: f32,
    filter: LowPass,
}

impl Voice {
    fn new(rate: u32, start: u64, tone: Tone) -> Self {
        let attack = sample_count(rate, tone.attack_ms);
        let decay = sample_count(rate, tone.decay_ms);
        let step = tone.pitch * TAU / rate as f32;
        let peak = tone.peak.max(GAIN_FLOOR);
        let sweep = (tone.kind == Kind::Flip).then(|| {
            let end = sample_count(rate, 70);
            Sweep {
                end,
                target: step * 0.72,
                multiplier: 0.72_f32.powf(1.0 / end as f32),
                filter: LowPass::new(rate, 1700.0),
            }
        });
        Self {
            start,
            end: start + u64::from(sample_count(rate, tone.stop_ms)),
            attack,
            decay,
            peak,
            gain: GAIN_FLOOR,
            rise: (peak / GAIN_FLOOR).powf(1.0 / attack as f32),
            fall: (GAIN_FLOOR / peak).powf(1.0 / (decay - attack) as f32),
            phase: 0.0,
            step,
            kind: tone.kind,
            sweep,
        }
    }

    fn sample(&mut self, frame: u64) -> Option<f32> {
        if frame < self.start {
            return Some(0.0);
        }
        if frame >= self.end {
            return None;
        }
        let age = (frame - self.start) as u32;
        if age == self.attack {
            self.gain = self.peak;
        } else if age == self.decay {
            self.gain = GAIN_FLOOR;
        }
        let mut signal = self.phase.sin();
        if let Some(sweep) = &mut self.sweep {
            signal = sweep.filter.process(signal);
            if age == sweep.end {
                self.step = sweep.target;
            }
        }
        self.phase += self.step;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        if let Some(sweep) = &self.sweep
            && age < sweep.end
        {
            self.step *= sweep.multiplier;
        }

        let out = signal * self.gain;
        if age < self.attack {
            self.gain *= self.rise;
        } else if age < self.decay {
            self.gain *= self.fall;
        }
        Some(out)
    }
}

struct Echo {
    line: Box<[f32]>,
    cursor: usize,
    filter: LowPass,
}

impl Echo {
    fn new(rate: u32) -> Self {
        Self {
            line: vec![0.0; sample_count(rate, 310) as usize].into_boxed_slice(),
            cursor: 0,
            filter: LowPass::new(rate, 1500.0),
        }
    }

    fn process(&mut self, wet: f32) -> f32 {
        let tap = self.filter.process(self.line[self.cursor]);
        self.line[self.cursor] = wet * 0.5 + tap * 0.38;
        self.cursor += 1;
        if self.cursor == self.line.len() {
            self.cursor = 0;
        }
        tap
    }

    fn clear(&mut self) {
        self.line.fill(0.0);
        self.cursor = 0;
        self.filter.z1 = 0.0;
        self.filter.z2 = 0.0;
    }
}

struct LowPass {
    b: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl LowPass {
    fn new(rate: u32, cutoff: f32) -> Self {
        let (sin, cos) = (TAU * cutoff / rate as f32).sin_cos();
        let alpha = sin / (2.0 * 10.0_f32.powf(1.0 / 20.0));
        let scale = 1.0 / (1.0 + alpha);
        Self {
            b: (1.0 - cos) * 0.5 * scale,
            a1: -2.0 * cos * scale,
            a2: (1.0 - alpha) * scale,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let b = self.b * input;
        let out = b + self.z1;
        self.z1 = 2.0 * b - self.a1 * out + self.z2;
        self.z2 = b - self.a2 * out;
        // Stop inaudible feedback from reaching slow subnormal values.
        if self.z1.abs() < 1e-15 && self.z2.abs() < 1e-15 {
            self.z1 = 0.0;
            self.z2 = 0.0;
        }
        out
    }
}

struct Compressor {
    gain: f32,
    attack: f32,
    release: f32,
}

impl Compressor {
    fn new(rate: u32) -> Self {
        Self {
            gain: 1.0,
            attack: (-1.0 / (rate as f32 * 0.003)).exp(),
            release: (-1.0 / (rate as f32 * 0.25)).exp(),
        }
    }

    fn process(&mut self, sample: f32) -> f32 {
        let level = sample.abs();
        let target = if level > 0.017_782_794 {
            // -26 dB threshold, 18 dB soft knee and 5:1 ratio.
            let over = 20.0 * level.log10() + 26.0;
            let compressed = if over < 9.0 {
                (over + 9.0).powi(2) / 36.0
            } else {
                over
            };
            10.0_f32.powf(compressed * (-0.8 / 20.0))
        } else {
            1.0
        };
        let coefficient = if target < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain = target + coefficient * (self.gain - target);
        (sample * self.gain).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn at(ms: u32) -> u64 {
        u64::from(sample_count(RATE, ms))
    }

    fn advance_to(synth: &mut Synth, frame: u64) -> f32 {
        let mut peak: f32 = 0.0;
        while synth.frame < frame {
            let sample = synth.sample();
            assert!(sample.is_finite());
            peak = peak.max(sample.abs());
        }
        peak
    }

    fn pitch(voice: &Voice) -> f32 {
        voice.step * RATE as f32 / TAU
    }

    #[test]
    fn flip_sweeps_down_and_collapses_bursts() {
        let mut synth = Synth::new(RATE);
        synth.handle(Command::Flip(1.0));
        let start_pitch = pitch(&synth.notes[0]);
        assert!((605.0..=677.0).contains(&start_pitch));

        let peak = advance_to(&mut synth, at(27));
        assert!((0.03..0.12).contains(&peak));
        synth.handle(Command::Flip(1.0));
        assert_eq!(synth.notes.len(), 1);

        advance_to(&mut synth, at(28));
        synth.handle(Command::Flip(1.0));
        assert_eq!(synth.notes.len(), 2);
        assert!(synth.notes[1].peak < synth.notes[0].peak);

        advance_to(&mut synth, at(71));
        assert!((pitch(&synth.notes[0]) - start_pitch * 0.72).abs() < 0.001);
        advance_to(&mut synth, at(220));
        assert!(synth.notes.is_empty());
        assert_eq!(synth.sample(), 0.0);
    }

    #[test]
    fn wave_ticks_are_evenly_spaced_and_get_quieter() {
        let mut synth = Synth::new(RATE);
        synth.handle(Command::Flip(1.0));
        synth.handle(Command::Wave(Duration::from_millis(160), 4));
        for k in 1..=4 {
            let frame = at(40 * k);
            assert_eq!(synth.next_tick, Some(frame));
            advance_to(&mut synth, frame);
            assert_ne!(synth.flips.last, Some(frame));
            synth.sample();
            assert_eq!(synth.flips.last, Some(frame));
            let latest_peak = synth
                .notes
                .iter()
                .find(|note| note.start == frame)
                .unwrap()
                .peak;
            assert!(latest_peak < 0.11 * 0.6 * 0.72_f32.powi(k as i32 - 1));
        }
        assert_eq!(synth.next_tick, None);
    }

    #[test]
    fn chime_changes_from_an_echoed_arpeggio_to_single_dry_notes() {
        let mut synth = Synth::new(RATE);
        synth.handle(Command::Completed);
        assert_eq!(synth.notes.len(), 4);
        for (k, note) in synth.notes.iter().enumerate() {
            assert_eq!(note.start, at(80 + 110 * k as u32));
            assert!((pitch(note) - CHIME_PITCHES[k]).abs() < 0.001);
            assert!(note.kind == Kind::Echoed);
        }
        assert_eq!(advance_to(&mut synth, at(80)), 0.0);

        advance_to(&mut synth, at(119));
        synth.handle(Command::Completed);
        assert_eq!(synth.notes.len(), 4);

        for k in 1..=2 {
            advance_to(&mut synth, at(120 * k));
            synth.handle(Command::Completed);
            assert_eq!(synth.notes.len(), 4 + k as usize);
            let note = synth.notes.last().unwrap();
            assert_eq!(note.start, at(120 * k + 60));
            assert!((pitch(note) - CHIME_PITCHES[k as usize - 1]).abs() < 0.001);
            assert!(note.kind == Kind::Dry);
        }

        advance_to(&mut synth, at(5_000));
        synth.handle(Command::Completed);
        assert_eq!(synth.notes.len(), 4);
        assert_eq!(synth.next_chime, 0);
    }

    #[test]
    fn item_rises_and_echo_has_a_delayed_feedback_tail() {
        let mut synth = Synth::new(RATE);
        synth.handle(Command::Item);
        assert_eq!(synth.notes.len(), 5);
        for (k, note) in synth.notes.iter().enumerate() {
            assert_eq!(note.start, at(50 + 90 * k as u32));
            assert!((pitch(note) - ITEM_PITCHES[k]).abs() < 0.001);
            assert!(note.kind == Kind::Echoed);
        }
        assert_eq!(advance_to(&mut synth, at(50)), 0.0);
        assert!(advance_to(&mut synth, at(1_000)) > 0.01);

        let mut echo = Echo::new(RATE);
        assert_eq!(echo.process(1.0), 0.0);
        for _ in 1..at(310) {
            assert_eq!(echo.process(0.0), 0.0);
        }
        let first = echo.process(0.0);
        assert!(first > 0.0);
        for _ in at(310) + 1..at(620) {
            echo.process(0.0);
        }
        let second = echo.process(0.0);
        assert!(second > 0.0 && second < first);
    }

    #[test]
    fn synth_drops_overflow_without_growing_storage() {
        let mut synth = Synth::new(RATE);
        let capacity = synth.notes.capacity();
        for _ in 0..128 {
            synth.handle(Command::Item);
            synth.handle(Command::Wave(Duration::from_millis(600), 4));
        }
        assert_eq!(synth.notes.len(), NOTE_LIMIT);
        assert_eq!(synth.notes.capacity(), capacity);
        assert_eq!(synth.ticks.iter().flatten().count(), WAVE_LIMIT);
        assert!(advance_to(&mut synth, at(800)) <= 1.0);
    }

    #[test]
    fn mute_clears_active_audio_and_rejects_stale_commands() {
        let (inbox, receiver) = mpsc::sync_channel(COMMAND_LIMIT);
        let generation = Arc::new(AtomicUsize::new(0));
        let mut callback = Callback {
            synth: Synth::new(RATE),
            receiver,
            generation: generation.clone(),
            current: 0,
        };
        let send = |generation, command| {
            inbox
                .try_send(Queued {
                    generation,
                    command,
                })
                .unwrap()
        };
        let mut samples = [0.0_f32; 4_800];

        send(0, Command::Flip(1.0));
        send(0, Command::Wave(Duration::from_millis(600), 4));
        send(0, Command::Item);
        callback.render(&mut samples, 1);
        assert!(samples.iter().any(|sample| *sample != 0.0));
        assert!(callback.synth.echo.line.iter().any(|sample| *sample != 0.0));

        generation.store(1, Ordering::Release);
        callback.render(&mut samples, 1);
        assert!(samples.iter().all(|sample| *sample == 0.0));
        assert!(callback.synth.notes.is_empty());
        assert_eq!(callback.synth.next_tick, None);
        assert!(callback.synth.echo.line.iter().all(|sample| *sample == 0.0));

        // Off and on can also occur entirely between callbacks.
        send(2, Command::Item);
        generation.store(4, Ordering::Release);
        send(4, Command::Flip(0.8));
        callback.render(&mut samples, 2);
        assert_eq!(callback.synth.notes.len(), 1);
        assert!(callback.synth.notes[0].kind == Kind::Flip);
        assert!(samples.iter().any(|sample| *sample != 0.0));
        assert!(samples.as_chunks::<2>().0.iter().all(|[l, r]| l == r));
    }
}
