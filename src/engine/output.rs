//! librespot `Sink` on top of rodio that survives missing and disappearing output devices, and
//! applies Coda's equalizer and fades.
//!
//! - The output stream is opened on `start` (the Windows default device at that moment) and
//!   closed on `stop`, so a paused Coda holds no audio device and costs no CPU, and pressing
//!   play after switching speakers/headphones uses the new default.
//! - Opening never panics and never fails: if there is no device the problem is reported and the
//!   output stays closed, so the first `write` fails and librespot pauses (see `start`).
//! - If the device stops taking samples (unplugged mid-song), `write` notices within
//!   `STALL_TIMEOUT` instead of waiting forever. It then waits up to `RETURN_WINDOW` for that same
//!   device to come back (a USB hub or DAC that resets itself drops out for a few seconds) and
//!   carries on there; only if it stays gone does the player pause. It never moves to another
//!   device by itself, so unplugged headphones don't continue on the speakers.
//! - Each packet is equalized in place as it is written (see [`Equalizer`]); fades are applied
//!   while it plays (see [`PacketSource`] and `gain`).
//! - librespot's audio is always 44.1 kHz. A YouTube song may have another rate: the equalizer is
//!   then designed for it, and rodio converts it to the device's rate like any other.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

use super::dsp::{EqControl, Equalizer};
use super::gain::OutputGain;
use super::lock;
use super::pcm::PcmSink;

/// About half a second of audio (librespot packets average ~1600 samples).
const MAX_QUEUED_CHUNKS: usize = 26;
/// No progress for this long means the device is gone.
const STALL_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long a device that stopped is waited for before the player pauses.
const RETURN_WINDOW: Duration = Duration::from_secs(8);
/// How often the waiting looks for the device.
const RETURN_POLL: Duration = Duration::from_millis(250);
/// The playing gain is re-read this often (in samples, ~1.5 ms) and glided to linearly.
const GAIN_BLOCK: u32 = 128;

const NO_DEVICE: &str = "No speakers or headphones were found. Connect an audio device and press play.";
const OPEN_FAILED: &str = "coda couldn't use your speakers or headphones. Check the Windows sound settings and press play.";
const DEVICE_LOST: &str = "Your speakers or headphones disconnected and didn't come back. Press play to continue on the ones Windows uses now.";

/// Last audio problem, in plain language, for the controller to show.
pub(super) type ProblemSlot = Arc<Mutex<Option<String>>>;

/// What an output shares with the engine.
pub(super) struct SinkParts {
    pub(super) problem: ProblemSlot,
    pub(super) gain: Arc<OutputGain>,
    pub(super) eq: Arc<EqControl>,
}

struct Output {
    queue: rodio::Sink,
    /// `None` only in tests.
    _stream: Option<rodio::OutputStream>,
    /// Set by the stream's error callback (device invalidated, driver error).
    failed: Arc<AtomicBool>,
    /// The device's name, to find it again if it drops out.
    device: Option<String>,
}

impl Output {
    /// Opens Windows' default output device.
    fn open() -> Result<Self, String> {
        let device = {
            use rodio::cpal::traits::HostTrait;
            rodio::cpal::default_host().default_output_device()
        };
        let name = device.as_ref().and_then(device_name);
        let builder = match device {
            Some(device) => rodio::OutputStreamBuilder::from_device(device),
            None => Err(rodio::StreamError::NoDevice),
        };
        let builder = builder.map_err(|e| {
            log::warn!("no usable audio output device: {e}");
            match e {
                rodio::StreamError::NoDevice => NO_DEVICE.to_owned(),
                _ => OPEN_FAILED.to_owned(),
            }
        })?;
        Self::open_with(builder, name)
    }

    /// Opens the output device called `name`, if it is there.
    fn reopen(name: &str) -> Option<Self> {
        use rodio::cpal::traits::HostTrait;
        let device = rodio::cpal::default_host()
            .output_devices()
            .ok()?
            .find(|d| device_name(d).as_deref() == Some(name))?;
        let builder = rodio::OutputStreamBuilder::from_device(device).ok()?;
        Self::open_with(builder, Some(name.to_owned())).ok()
    }

    fn open_with(builder: rodio::OutputStreamBuilder, device: Option<String>) -> Result<Self, String> {
        let failed = Arc::new(AtomicBool::new(false));
        let flag = failed.clone();
        let builder = builder.with_error_callback(move |e: rodio::cpal::StreamError| {
            log::warn!("audio output error: {e}");
            flag.store(true, Ordering::Release);
        });
        let mut stream = builder.open_stream_or_fallback().map_err(|e| {
            log::warn!("could not open the audio output: {e}");
            OPEN_FAILED.to_owned()
        })?;
        stream.log_on_drop(false);
        let queue = rodio::Sink::connect_new(stream.mixer());
        log::debug!("audio output opened on {device:?} ({:?})", stream.config());
        Ok(Self { queue, _stream: Some(stream), failed, device })
    }

    /// Waits until at most `max_chunks` packets are queued. Returns `false` if the device
    /// stopped consuming audio.
    fn wait_until_queued_at_most(&self, max_chunks: usize) -> bool {
        self.wait_until_queued_at_most_within(max_chunks, STALL_TIMEOUT)
    }

    fn wait_until_queued_at_most_within(&self, max_chunks: usize, stall_timeout: Duration) -> bool {
        let mut last_len = usize::MAX;
        let mut last_progress = Instant::now();
        loop {
            let len = self.queue.len();
            if len <= max_chunks {
                return true;
            }
            if self.failed.load(Ordering::Acquire) {
                return false;
            }
            if len < last_len {
                last_len = len;
                last_progress = Instant::now();
            } else if last_progress.elapsed() >= stall_timeout {
                return false;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn device_name(device: &rodio::cpal::Device) -> Option<String> {
    use rodio::cpal::traits::DeviceTrait;
    device.name().ok()
}

/// How long `CodaSink` waits for a device (shortened in tests).
struct Timing {
    stall: Duration,
    return_window: Duration,
    return_poll: Duration,
}

const TIMING: Timing = Timing { stall: STALL_TIMEOUT, return_window: RETURN_WINDOW, return_poll: RETURN_POLL };

pub(super) struct CodaSink {
    output: Option<Output>,
    problem: ProblemSlot,
    gain: Arc<OutputGain>,
    eq_control: Arc<EqControl>,
    eq: Equalizer,
    /// Samples per second of the packets written.
    rate: u32,
    /// Opens the output device (`Output::open`; replaced in tests).
    open: fn() -> Result<Output, String>,
    /// Opens a named device again after it dropped out (`Output::reopen`; replaced in tests).
    reopen: fn(&str) -> Option<Output>,
    timing: Timing,
}

impl CodaSink {
    pub(super) fn new(parts: SinkParts) -> Self {
        Self::with_opener(parts, Output::open)
    }

    fn with_opener(parts: SinkParts, open: fn() -> Result<Output, String>) -> Self {
        let eq = Equalizer::new(&parts.eq);
        Self {
            output: None,
            problem: parts.problem,
            gain: parts.gain,
            eq_control: parts.eq,
            eq,
            rate: SAMPLE_RATE,
            open,
            reopen: Output::reopen,
            timing: TIMING,
        }
    }

    /// Waits up to the return window for the device called `name` to be back, and opens it.
    fn wait_for_return(&self, name: &str) -> Option<Output> {
        let deadline = Instant::now() + self.timing.return_window;
        loop {
            thread::sleep(self.timing.return_poll);
            if let Some(output) = (self.reopen)(name) {
                return Some(output);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// The packets written from now on have `rate` samples per second.
    fn set_rate(&mut self, rate: u32) {
        if rate != self.rate && rate > 0 {
            self.rate = rate;
            self.eq = Equalizer::with_rate(&self.eq_control, f64::from(rate));
        }
    }

    fn report(&self, message: &str) {
        *lock(&self.problem) = Some(message.to_owned());
    }
}

impl Sink for CodaSink {
    fn start(&mut self) -> SinkResult<()> {
        if self.output.as_ref().is_some_and(|o| o.failed.load(Ordering::Acquire)) {
            self.output = None;
        }
        if self.output.is_none() {
            match (self.open)() {
                Ok(output) => {
                    self.output = Some(output);
                    lock(&self.problem).take();
                }
                // Never fail here. While librespot starts a track that should play, it calls
                // `start` twice (once still loading, once playing) and handles a second
                // failure by ending the process ("Invalid PlayerState", `exit(1)`). Staying
                // closed is safe instead: the first `write` then fails with `NotConnected`,
                // which librespot answers by pausing (and reporting `Paused`), exactly as
                // when a device disappears mid-song.
                Err(message) => self.report(&message),
            }
        }
        Ok(())
    }

    // Must never fail: librespot exits the process when `stop` returns an error.
    fn stop(&mut self) -> SinkResult<()> {
        if let Some(output) = self.output.take() {
            // Let the queued ~0.5 s play out so pausing doesn't skip audio on resume.
            if !output.wait_until_queued_at_most(0) {
                log::debug!("audio device stopped while finishing playback");
            }
        }
        // Whatever plays next starts from silence (a new output), so the equalizer starts cold
        // too: the old filter memory would otherwise begin the next track with a thump.
        self.eq.reset();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let healthy = {
            let Some(output) = self.output.as_ref() else {
                return Err(SinkError::NotConnected("the audio output is closed".into()));
            };
            let mut samples = match packet {
                AudioPacket::Samples(samples) => samples,
                AudioPacket::Raw(_) => return Err(SinkError::OnWrite("raw audio packets are not supported".into())),
            };
            if samples.is_empty() {
                return Ok(());
            }
            self.eq.sync(&self.eq_control);
            self.eq.process(&mut samples);
            output.queue.append(PacketSource::new(converter.f64_to_f32(&samples), self.gain.clone(), self.rate));
            output.wait_until_queued_at_most_within(MAX_QUEUED_CHUNKS, self.timing.stall)
        };
        if healthy {
            return Ok(());
        }
        let device = self.output.take().and_then(|o| o.device);
        if let Some(name) = device {
            log::warn!("the audio device {name:?} stopped taking samples; waiting for it to come back");
            if let Some(output) = self.wait_for_return(&name) {
                // The ~0.5 s queued on the old output is lost; the song goes on from here.
                log::info!("the audio device {name:?} is back; playing on");
                self.eq.reset();
                self.output = Some(output);
                return Ok(());
            }
        }
        log::warn!("the audio device stopped taking samples and didn't come back; pausing");
        self.report(DEVICE_LOST);
        Err(SinkError::OnWrite("the audio device stopped responding".into()))
    }
}

/// The same output for Coda's own player, whose songs may have another rate.
pub(super) struct PcmOutput(CodaSink);

impl PcmOutput {
    pub(super) fn new(parts: SinkParts) -> Self {
        Self(CodaSink::new(parts))
    }
}

impl PcmSink for PcmOutput {
    fn start(&mut self) -> SinkResult<()> {
        self.0.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.0.stop()
    }

    fn write(&mut self, samples: Vec<f64>, rate: u32) -> SinkResult<()> {
        self.0.set_rate(rate);
        self.0.write(AudioPacket::Samples(samples), &mut Converter::new(None))
    }
}

/// One decoded packet as a rodio source. Its gain is looked up while it plays (every
/// `GAIN_BLOCK` samples) and glided to sample by sample, so fades are heard the moment they are
/// set and never step.
pub(super) struct PacketSource {
    samples: Vec<f32>,
    rate: u32,
    pos: usize,
    gain: Arc<OutputGain>,
    factor: f32,
    step: f32,
    until_update: u32,
    /// The first gain lookup sets `factor` directly instead of gliding to it.
    started: bool,
}

impl PacketSource {
    pub(super) fn new(samples: Vec<f32>, gain: Arc<OutputGain>, rate: u32) -> Self {
        Self { samples, rate, pos: 0, gain, factor: 1.0, step: 0.0, until_update: 0, started: false }
    }

    fn update_gain(&mut self) {
        self.until_update = GAIN_BLOCK;
        let Some(target) = self.gain.current() else {
            // The envelope is being changed right now: keep the current gain for one block. At
            // the start of the packet there is none yet; the last known one is close (it was
            // read by the block before this one, or is where the engine's change starts from).
            if !self.started {
                self.started = true;
                self.factor = self.gain.last_known();
            }
            self.step = 0.0;
            return;
        };
        if !self.started {
            self.started = true;
            self.factor = target;
            self.step = 0.0;
        } else {
            self.step = (target - self.factor) / GAIN_BLOCK as f32;
        }
    }
}

impl Iterator for PacketSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let sample = *self.samples.get(self.pos)?;
        self.pos += 1;
        if self.until_update == 0 {
            self.update_gain();
        }
        self.until_update -= 1;
        self.factor += self.step;
        Some(sample * self.factor)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.samples.len() - self.pos;
        (left, Some(left))
    }
}

impl rodio::Source for PacketSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::from(NUM_CHANNELS)
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        let frames = (self.samples.len() / usize::from(NUM_CHANNELS)) as u64;
        Some(Duration::from_nanos(frames * 1_000_000_000 / u64::from(self.rate.max(1))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::gain::{Curve, GainControl};
    use crate::model::EqSettings;

    fn no_device() -> Result<Output, String> {
        Err(NO_DEVICE.to_owned())
    }

    /// An output nothing plays from, like a device that stopped taking samples.
    fn stuck_output(device: &str) -> Output {
        let (queue, _never_played) = rodio::Sink::new();
        Output { queue, _stream: None, failed: Arc::new(AtomicBool::new(false)), device: Some(device.to_owned()) }
    }

    fn stuck_headphones() -> Result<Output, String> {
        Ok(stuck_output("Headphones (USB DAC)"))
    }

    const FAST: Timing = Timing {
        stall: Duration::from_millis(100),
        return_window: Duration::from_millis(300),
        return_poll: Duration::from_millis(20),
    };

    /// Fills a stuck output past the queue limit, so the next packet finds it stalled.
    fn write_until_stalled(sink: &mut CodaSink) -> SinkResult<()> {
        let mut converter = Converter::new(None);
        for _ in 0..=MAX_QUEUED_CHUNKS {
            sink.write(AudioPacket::Samples(vec![0.0; 64]), &mut converter)?;
        }
        Ok(())
    }

    /// A USB DAC whose hub resets drops off for a few seconds and comes back under the same
    /// name: the song keeps playing there, and no problem is shown.
    #[test]
    fn a_device_that_drops_out_and_comes_back_keeps_playing() {
        let problem = ProblemSlot::default();
        let mut sink = CodaSink::with_opener(parts(problem.clone()), stuck_headphones);
        sink.timing = FAST;
        sink.reopen = |name| (name == "Headphones (USB DAC)").then(|| stuck_output(name));
        sink.start().expect("start");
        write_until_stalled(&mut sink).expect("the stall is ridden out on the returned device");
        assert!(sink.output.is_some(), "playing on the device that came back");
        assert_eq!(lock(&problem).as_deref(), None, "nothing to tell the person");
    }

    /// Unplugged for good: the player pauses with the reason, and never moves on to another
    /// device by itself.
    #[test]
    fn a_device_that_stays_gone_pauses_with_the_reason() {
        let problem = ProblemSlot::default();
        let mut sink = CodaSink::with_opener(parts(problem.clone()), stuck_headphones);
        sink.timing = FAST;
        sink.reopen = |_| None;
        sink.start().expect("start");
        let started = Instant::now();
        let written = write_until_stalled(&mut sink);
        assert!(matches!(written, Err(SinkError::OnWrite(_))), "{written:?}");
        assert!(started.elapsed() >= FAST.stall + FAST.return_window, "waited for it first");
        assert!(sink.output.is_none());
        assert_eq!(lock(&problem).as_deref(), Some(DEVICE_LOST));
        sink.stop().expect("stop after the failed write");
    }

    fn parts(problem: ProblemSlot) -> SinkParts {
        SinkParts {
            problem,
            gain: Arc::new(OutputGain::new(Arc::new(GainControl::new(1.0)))),
            eq: Arc::new(EqControl::new(&EqSettings::default())),
        }
    }

    /// The sequence librespot 0.8 runs for `load(track, start_playing = true, ..)` while the
    /// output can't be opened (no device, or a device held in exclusive mode). Its load path
    /// calls `start` once while the track is still loading and once more when it starts
    /// playing; a second failure makes it call `exit(1)`. So `start` must succeed both times,
    /// and the first packet must fail with `NotConnected`, which librespot turns into a pause.
    #[test]
    fn starting_without_a_device_pauses_instead_of_failing() {
        let problem = ProblemSlot::default();
        let mut sink = CodaSink::with_opener(parts(problem.clone()), no_device);
        sink.start().expect("start while loading");
        sink.start().expect("start when playback begins");
        let mut converter = Converter::new(None);
        let written = sink.write(AudioPacket::Samples(vec![0.0; 2048]), &mut converter);
        assert!(matches!(written, Err(SinkError::NotConnected(_))), "{written:?}");
        // librespot's pause stops the sink; that must not fail either.
        sink.stop().expect("stop after the failed write");
        assert_eq!(lock(&problem).as_deref(), Some(NO_DEVICE), "the reason is kept for the controller");
        // Pressing play again tries the device again, still without failing.
        sink.start().expect("start on the next play");
        assert!(sink.write(AudioPacket::Samples(vec![0.0; 16]), &mut converter).is_err());
    }

    fn packet(gain: &Arc<OutputGain>, len: usize) -> PacketSource {
        PacketSource::new(vec![1.0; len], gain.clone(), SAMPLE_RATE)
    }

    #[test]
    fn packets_play_at_unity_untouched() {
        let gain = Arc::new(OutputGain::new(Arc::new(GainControl::new(1.0))));
        let played: Vec<f32> = packet(&gain, 1000).collect();
        assert_eq!(played, vec![1.0; 1000]);
        let source = packet(&gain, 44_100 * 2);
        assert_eq!(rodio::Source::total_duration(&source), Some(Duration::from_secs(1)));
        assert_eq!(rodio::Source::channels(&source), 2);
    }

    #[test]
    fn packets_use_the_gain_at_play_time_and_glide_to_changes() {
        let global = Arc::new(GainControl::new(1.0));
        let gain = Arc::new(OutputGain::new(global.clone()));
        // Made (queued) at full gain, but the fade is set before it plays.
        let mut source = packet(&gain, 4 * GAIN_BLOCK as usize);
        gain.own.set(0.5);
        global.set(0.5);
        assert_eq!(source.next(), Some(0.25), "the gain when playing starts, without a glide");
        // A change while playing is reached within one block, in small steps.
        gain.own.set(1.0);
        let rest: Vec<f32> = source.by_ref().take(2 * GAIN_BLOCK as usize).collect();
        let steps = rest.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(steps < 0.01, "largest step {steps}");
        assert!((rest.last().copied().unwrap() - 0.5).abs() < 1e-4, "{:?}", rest.last());
    }

    #[test]
    fn a_busy_envelope_at_the_start_of_a_packet_keeps_the_last_known_gain() {
        let gain = Arc::new(OutputGain::new(Arc::new(GainControl::new(1.0))));
        // A crossfade's incoming output, silent so far; the engine is changing its envelope.
        gain.own.set(0.0);
        let held = gain.own.hold();
        let mut source = packet(&gain, 4 * GAIN_BLOCK as usize);
        let first: Vec<f32> = source.by_ref().take(GAIN_BLOCK as usize).collect();
        assert!(first.iter().all(|&s| s == 0.0), "played {:?} instead of silence", &first[..4]);
        drop(held);
        // The fade-in then glides up from there.
        gain.own.set(1.0);
        let next: Vec<f32> = source.take(GAIN_BLOCK as usize).collect();
        assert!(next[0] < 0.05 && next.windows(2).all(|w| w[1] >= w[0]), "{:?}", &next[..4]);
    }

    #[test]
    fn stopping_forgets_the_equalizer_state() {
        let mut eq_settings = EqSettings { enabled: true, preamp_db: -6.0, ..EqSettings::default() };
        eq_settings.gains_db[..3].copy_from_slice(&[6.0, 5.0, 4.0]);
        let mut parts = parts(ProblemSlot::default());
        parts.eq = Arc::new(EqControl::new(&eq_settings));
        let mut sink = CodaSink::with_opener(parts, no_device);
        let mut bass: Vec<f64> =
            (0..44_100).flat_map(|n| [0.3 * (2.0 * std::f64::consts::PI * 50.0 * f64::from(n) / 44_100.0).sin(); 2]).collect();
        sink.eq.process(&mut bass);
        sink.stop().expect("stop never fails");
        // The next track starts in digital silence, and so does the output.
        let mut silence = vec![0.0; 64];
        sink.eq.process(&mut silence);
        assert!(silence.iter().all(|&s| s == 0.0), "{:?}", &silence[..4]);
    }

    #[test]
    fn another_rate_plays_at_its_own_speed_and_redesigns_the_equalizer() {
        let mut sink = CodaSink::with_opener(parts(ProblemSlot::default()), no_device);
        assert_eq!(sink.eq.rate(), f64::from(SAMPLE_RATE));
        sink.set_rate(48_000);
        assert_eq!(sink.eq.rate(), 48_000.0);
        sink.set_rate(0);
        assert_eq!(sink.rate, 48_000, "a rate of 0 is ignored");
        let gain = Arc::new(OutputGain::new(Arc::new(GainControl::new(1.0))));
        let source = PacketSource::new(vec![0.0; 96_000], gain, 48_000);
        assert_eq!(rodio::Source::sample_rate(&source), 48_000);
        assert_eq!(rodio::Source::total_duration(&source), Some(Duration::from_secs(1)));
    }

    #[test]
    fn a_fade_out_reaches_silence() {
        let gain = Arc::new(OutputGain::new(Arc::new(GainControl::new(1.0))));
        gain.own.ramp_to(0.0, Duration::from_millis(5), Curve::EqualPower);
        std::thread::sleep(Duration::from_millis(10));
        assert!(packet(&gain, 512).all(|s| s == 0.0));
    }
}
