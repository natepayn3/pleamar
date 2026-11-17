//! Sound, spoken to directly: PulseAudio's protocol over its socket, which
//! PipeWire speaks too (pipewire-pulse). Nothing is spawned: no `wpctl`, no
//! `pactl subscribe` underneath. One connection subscribes to the server's
//! events and reports on each; commands open their own, short, one.
//!
//! `{ volume, muted, input, input_muted, outputs, inputs, apps }`: the default
//! output and input (volume as the mixer shows it, 0..1, 1 being 100 %), the
//! devices for each, `{ { id, name, default }, … }` (the monitors of the
//! outputs are not inputs), and what is playing, one per stream:
//! `{ { id, name, icon, binary, title, volume, muted, playing }, … }`.

use super::SysValue;
use pulseaudio::protocol::{self, ChannelVolume, Command, Prop, Props, SetDeviceMuteParams, SetDeviceVolumeParams, SetStreamMuteParams, SetStreamVolumeParams, SinkInfo, SinkInputInfo, SourceInfo, Volume};
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;

/// A connection to the sound server, authenticated and named.
struct Pulse {
    sock: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

type R<T> = Result<T, String>;

impl Pulse {
    fn connect(name: &str) -> R<Pulse> {
        let path = pulseaudio::socket_path_from_env().ok_or("no sound server")?;
        let mut sock = BufReader::new(UnixStream::connect(path).map_err(|e| e.to_string())?);
        let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
        let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
        protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION).map_err(|e| e.to_string())?;
        let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION).map_err(|e| e.to_string())?;
        let version = protocol::MAX_VERSION.min(reply.version);
        let mut p = Pulse { sock, version, seq: 1 };
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, CString::new(name).unwrap_or_default());
        p.ask::<protocol::SetClientNameReply>(Command::SetClientName(props))?;
        Ok(p)
    }

    fn send(&mut self, c: &Command) -> R<()> {
        self.seq += 1;
        protocol::write_command_message(self.sock.get_mut(), self.seq, c, self.version).map_err(|e| e.to_string())
    }

    fn ask<T: protocol::CommandReply>(&mut self, c: Command) -> R<T> {
        self.send(&c)?;
        protocol::read_reply_message::<T>(&mut self.sock, self.version).map(|(_, t)| t).map_err(|e| e.to_string())
    }

    /// `ask`, but for no longer than `wait`: `None` when the server said nothing in that time.
    /// A read with no limit waits for ever on a server that never answers (a sink that went
    /// away as the question was asked), and the connection is no use after a reply missed:
    /// the caller drops it.
    fn ask_within<T: protocol::CommandReply>(&mut self, c: Command, wait: std::time::Duration) -> R<Option<T>> {
        self.sock.get_ref().set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
        let started = std::time::Instant::now();
        match self.ask::<T>(c) {
            Ok(t) => Ok(Some(t)),
            Err(_) if started.elapsed() >= wait.mul_f32(0.9) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn ack(&mut self, c: Command) -> R<()> {
        self.send(&c)?;
        protocol::read_ack_message(&mut self.sock).map(|_| ()).map_err(|e| e.to_string())
    }

    fn state(&mut self) -> R<(protocol::ServerInfo, Vec<SinkInfo>, Vec<SourceInfo>)> {
        let server = self.ask::<protocol::ServerInfo>(Command::GetServerInfo)?;
        let sinks = self.ask::<protocol::SinkInfoList>(Command::GetSinkInfoList)?;
        let sources = self.ask::<protocol::SourceInfoList>(Command::GetSourceInfoList)?;
        Ok((server, sinks, sources))
    }

    /// The streams that play, each an application's (a browser tab, a game, a
    /// call). Without one to say so, nothing: the devices are still told.
    fn apps(&mut self) -> Vec<SinkInputInfo> {
        self.ask::<protocol::SinkInputInfoList>(Command::GetSinkInputInfoList).unwrap_or_default()
    }
}

/// The volume as the mixer shows it: the loudest channel, 1 being 100 %.
fn level(v: &ChannelVolume) -> f64 {
    v.channels().iter().map(|c| c.as_u32()).max().unwrap_or(0) as f64 / Volume::NORM.as_u32() as f64
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn name_of(description: &Option<CString>, name: &CString) -> String {
    description.as_ref().unwrap_or(name).to_string_lossy().into_owned()
}

/// A property of a stream, as text: they come with their final zero.
fn prop(props: &Props, p: Prop) -> Option<String> {
    let bytes = props.get(p)?;
    let text = String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// One stream that plays, as the logic sees it. Event sounds (a click, a
/// notice's chime) come and go in a second and are nobody's to set: left out,
/// as are streams whose volume cannot be read.
fn app(i: &SinkInputInfo) -> Option<SysValue> {
    if !i.has_volume || prop(&i.props, Prop::MediaRole).as_deref() == Some("event") {
        return None;
    }
    let binary = prop(&i.props, Prop::ApplicationProcessBinary).unwrap_or_default();
    let name = prop(&i.props, Prop::ApplicationName).unwrap_or_else(|| if binary.is_empty() { i.name.to_string_lossy().into_owned() } else { binary.clone() });
    let icon = prop(&i.props, Prop::ApplicationIconName).unwrap_or_else(|| binary.to_lowercase());
    Some(SysValue::Map(vec![
        ("id".into(), SysValue::Num(i.index as f64)),
        ("name".into(), SysValue::Text(name)),
        ("icon".into(), SysValue::Text(icon)),
        ("binary".into(), SysValue::Text(binary)),
        ("title".into(), SysValue::Text(prop(&i.props, Prop::MediaName).unwrap_or_default())),
        ("volume".into(), SysValue::Num(round(level(&i.cvolume)))),
        ("muted".into(), SysValue::Bool(i.muted)),
        ("playing".into(), SysValue::Bool(!i.corked)),
    ]))
}

fn report(server: &protocol::ServerInfo, sinks: &[SinkInfo], sources: &[SourceInfo], apps: &[SinkInputInfo]) -> SysValue {
    let default_sink = server.default_sink_name.as_ref();
    let default_source = server.default_source_name.as_ref();
    let sink = sinks.iter().find(|s| Some(&s.name) == default_sink).or(sinks.first());
    let inputs: Vec<&SourceInfo> = sources.iter().filter(|s| s.monitor_of_sink_index.is_none()).collect();
    let source = inputs.iter().copied().find(|s| Some(&s.name) == default_source).or(inputs.first().copied());
    let device = |id: u32, name: String, default: bool| SysValue::Map(vec![("id".into(), SysValue::Num(id as f64)), ("name".into(), SysValue::Text(name)), ("default".into(), SysValue::Bool(default))]);
    SysValue::Map(vec![
        ("volume".into(), SysValue::Num(sink.map_or(0.0, |s| round(level(&s.cvolume))))),
        ("muted".into(), SysValue::Bool(sink.is_some_and(|s| s.muted))),
        // With no microphone the service exists all the same: what is missing is the input.
        ("input".into(), SysValue::Num(source.map_or(0.0, |s| round(level(&s.cvolume))))),
        ("input_muted".into(), SysValue::Bool(source.is_none_or(|s| s.muted))),
        ("outputs".into(), SysValue::List(sinks.iter().map(|s| device(s.index, name_of(&s.description, &s.name), Some(&s.name) == default_sink)).collect())),
        ("inputs".into(), SysValue::List(inputs.iter().map(|s| device(s.index, name_of(&s.description, &s.name), Some(&s.name) == default_source)).collect())),
        ("apps".into(), SysValue::List(apps.iter().filter_map(app).collect())),
    ])
}

/// Whether there is a sound server to speak to.
pub fn available() -> bool {
    Pulse::connect("pleamar").is_ok()
}

pub fn service(dispatch: Box<dyn Fn(SysValue) + Send>) -> bool {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let mut last = String::new();
            loop {
                // Two connections: one hears the server's events, the other asks.
                let mut run = || -> R<()> {
                    let mut asker = Pulse::connect("pleamar")?;
                    let mut events = Pulse::connect("pleamar events")?;
                    events.ack(Command::Subscribe(protocol::SubscriptionMask::SINK | protocol::SubscriptionMask::SOURCE | protocol::SubscriptionMask::SINK_INPUT | protocol::SubscriptionMask::SERVER))?;
                    let mut tell = |asker: &mut Pulse| -> R<()> {
                        let (server, sinks, sources) = asker.state()?;
                        let apps = asker.apps();
                        let v = report(&server, &sinks, &sources, &apps);
                        let fingerprint = format!("{v:?}");
                        if fingerprint != last {
                            last = fingerprint;
                            dispatch(v);
                        }
                        Ok(())
                    };
                    tell(&mut asker)?;
                    loop {
                        protocol::read_command_message(&mut events.sock, events.version).map_err(|e| e.to_string())?;
                        // A volume slider dragged is a stream of events: read what else
                        // is already waiting, and tell once.
                        events.sock.get_ref().set_nonblocking(true).ok();
                        while protocol::read_command_message(&mut events.sock, events.version).is_ok() {}
                        events.sock.get_ref().set_nonblocking(false).ok();
                        tell(&mut asker)?;
                    }
                };
                if let Err(e) = run() {
                    eprintln!("audio · the sound server went away ({e}): trying again");
                }
                // Restarted (or not up yet): ask again in a moment.
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .is_ok()
}

/// How often the meter is read: the server measures the loudest sample of
/// each 1/30 s and sends that one number, nothing else. A screen's frame is
/// as often as a meter needs.
const LEVEL_RATE: u32 = 30;

/// The loudest of what came, as 0..1: a packet is float samples, one per
/// reading, little-endian.
fn peak_of(bytes: &[u8]) -> f32 {
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b).abs()).fold(0.0, f32::max).min(1.0)
}

/// What is told, and when: the loudest since the last time told, about
/// `LEVEL_RATE` times a second, and only when it is not what was told before
/// (a silence is said once, not thirty times a second). The server's
/// readings come thirty a second on average but each on the edge of its own
/// cycle: PipeWire's is 1024 samples at 48 kHz, so they come 21 or 43 ms
/// apart. Waiting a whole thirtieth between two would drop a third of
/// them; half of one lets those through, and still turns a burst
/// (readings held back and handed over together) into its loudest.
struct Meter {
    peak: f32,
    told: Option<f64>,
    when: Option<std::time::Instant>,
}

impl Meter {
    fn new() -> Meter {
        Meter { peak: 0.0, told: None, when: None }
    }

    fn feed(&mut self, peak: f32, now: std::time::Instant) -> Option<f64> {
        self.peak = self.peak.max(peak);
        let every = std::time::Duration::from_secs(1) / (2 * LEVEL_RATE);
        if self.when.is_some_and(|w| now.duration_since(w) < every) {
            return None;
        }
        self.when = Some(now);
        let v = round(std::mem::take(&mut self.peak) as f64);
        (self.told != Some(v)).then(|| {
            self.told = Some(v);
            v
        })
    }

    /// Nothing is playing, or the stream is gone or not there yet: 0, now.
    /// A peak still held back for the cap is dropped, not told after it, and
    /// the cap does not delay the 0. Told unless 0 is what was told last.
    fn silence(&mut self) -> Option<f64> {
        self.peak = 0.0;
        self.when = None;
        (self.told != Some(0.0)).then(|| {
            self.told = Some(0.0);
            0.0
        })
    }
}

/// The longest packet taken from the server. A reading is 4 bytes and an
/// event a few dozen; a length past this is a stream out of step, not data.
const LEVEL_PACKET_MAX: u32 = 1 << 20;

/// The longest the server is waited for to answer, before the stream and for it. An output
/// that is suspended may answer late, so this is long: a miss is not an error, the stream
/// is asked for again on a new connection (see `level_service`).
const LEVEL_REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

fn tell_level(dispatch: &dyn Fn(SysValue), v: Option<f64>) {
    if let Some(v) = v {
        dispatch(SysValue::Map(vec![("level".into(), SysValue::Num(v))]));
    }
}

/// `audio.level`: how loud what plays is, `{ level }`, 0..1, the loudest
/// sample of each moment on the default output's monitor. It is a signal,
/// not the mixer's volume: silence is 0 whatever the slider says. It is
/// what pavucontrol's meters do: a record stream with PEAK_DETECT, at a
/// rate of 30 readings a second, so the server does the measuring and
/// sends one number each time. It runs only once a scene asks for it
/// (`service audio.level`, `sys.watch("audio.level")`), and lets an idle
/// output suspend all the same. When the default output changes, it moves
/// with it.
pub fn level_service(dispatch: Box<dyn Fn(SysValue) + Send>) -> bool {
    std::thread::Builder::new()
        .name("audio level".into())
        .spawn(move || {
            let mut meter = Meter::new();
            // Silence until told otherwise: with the output suspended, the
            // server answers the stream only once something plays.
            tell_level(&*dispatch, meter.silence());
            loop {
                let mut run = || -> R<()> {
                    let mut p = Pulse::connect("pleamar level")?;
                    // The server may never answer (pleamar#164: an output that went away
                    // as the stream was asked for): no wait is for ever.
                    p.sock.get_ref().set_read_timeout(Some(LEVEL_REPLY_WAIT)).map_err(|e| e.to_string())?;
                    let (server, sinks, _) = p.state()?;
                    // With no default output, the first one; a default named
                    // later differs from none, so it is followed all the same.
                    let sink = sinks.iter().find(|s| Some(&s.name) == server.default_sink_name.as_ref()).or(sinks.first()).ok_or("there is no output")?;
                    let monitor = sink.monitor_source_index.ok_or("the output has no monitor")?;
                    let mut props = Props::new();
                    props.set(Prop::MediaName, CString::new("level").unwrap_or_default());
                    let Some(stream) = p.ask_within::<protocol::CreateRecordStreamReply>(Command::CreateRecordStream(protocol::RecordStreamParams {
                        source_index: Some(monitor),
                        sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels: 1, sample_rate: LEVEL_RATE },
                        channel_map: protocol::ChannelMap::mono(),
                        // One reading per packet: as soon as it is measured.
                        buffer_attr: protocol::stream::BufferAttr { fragment_size: 4, ..Default::default() },
                        flags: protocol::stream::StreamFlags { peak_detect: true, adjust_latency: true, no_move: true, no_inhibit_auto_suspend: true, ..Default::default() },
                        props,
                        ..Default::default()
                    }), LEVEL_REPLY_WAIT)? else {
                        // No answer in time: not told as a failure (an idle output may answer
                        // only once something plays). The stream is asked for again, on a new
                        // connection, with no pause: this one cannot be read on.
                        return Ok(());
                    };
                    tell_level(&*dispatch, meter.silence());
                    // Told when the default output changes, on this connection, asked
                    // last: an event coming while a reply is awaited would be taken for
                    // the reply. Its answer is read with the readings, below.
                    p.send(&Command::Subscribe(protocol::SubscriptionMask::SERVER))?;
                    // Whether the default is still the one, asked on a second
                    // connection: this one carries the readings.
                    let mut asker: Option<Pulse> = None;
                    let moved = |asker: &mut Option<Pulse>| -> R<bool> {
                        let a = match asker {
                            Some(a) => a,
                            None => asker.insert(Pulse::connect("pleamar level")?),
                        };
                        Ok(a.ask::<protocol::ServerInfo>(Command::GetServerInfo)?.default_sink_name != server.default_sink_name)
                    };
                    // Nothing comes while the output is suspended or nothing plays
                    // through it: that is a silence, and is told as one. It is only
                    // a silence between two packets; a packet cut short is an error.
                    p.sock.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(250))).ok();
                    let mut buf = Vec::new();
                    loop {
                        match std::io::BufRead::fill_buf(&mut p.sock) {
                            Ok([]) => return Err("the sound server closed the connection".into()),
                            Ok(_) => {}
                            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                                tell_level(&*dispatch, meter.silence());
                                continue;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(e) => return Err(e.to_string()),
                        }
                        let desc = protocol::read_descriptor(&mut p.sock).map_err(|e| e.to_string())?;
                        if desc.length > LEVEL_PACKET_MAX {
                            return Err(format!("a packet of {} bytes", desc.length));
                        }
                        buf.resize(desc.length as usize, 0);
                        std::io::Read::read_exact(&mut p.sock, &mut buf).map_err(|e| e.to_string())?;
                        if desc.channel == stream.channel {
                            tell_level(&*dispatch, meter.feed(peak_of(&buf), std::time::Instant::now()));
                            continue;
                        }
                        if desc.channel != u32::MAX {
                            continue;
                        }
                        match Command::read_tag_prefixed(&mut std::io::Cursor::new(&buf), p.version) {
                            // Subscribed (from now on a change is an event, and one made
                            // since the default was read shows now), or the server changed:
                            // if the default output is another, start again on it.
                            Ok((_, Command::Reply | Command::SubscribeEvent(_))) => {
                                if moved(&mut asker)? {
                                    return Ok(());
                                }
                            }
                            Ok((_, Command::Error(e))) => return Err(format!("not subscribed: {e:?}")),
                            Ok((_, Command::RecordStreamKilled(_))) => return Err("the output went away".into()),
                            _ => {}
                        }
                    }
                };
                let result = run();
                tell_level(&*dispatch, meter.silence());
                if let Err(e) = result {
                    eprintln!("audio.level · {e}: trying again");
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        })
        .is_ok()
}

fn set_volume(p: &mut Pulse, sink: bool, index: u32, channels: usize, v: f64) -> R<()> {
    let mut cv = ChannelVolume::empty();
    let raw = Volume::from_u32_clamped((v.clamp(0.0, 1.0) * Volume::NORM.as_u32() as f64).round() as u32);
    for _ in 0..channels.max(1) {
        cv.push(raw);
    }
    let params = SetDeviceVolumeParams { device_index: Some(index), device_name: None, volume: cv };
    p.ack(if sink { Command::SetSinkVolume(params) } else { Command::SetSourceVolume(params) })
}

/// One application's stream, as loud as `v` on every channel it has.
fn set_app_volume(p: &mut Pulse, index: u32, channels: usize, v: f64) -> R<()> {
    let mut cv = ChannelVolume::empty();
    let raw = Volume::from_u32_clamped((v.clamp(0.0, 1.0) * Volume::NORM.as_u32() as f64).round() as u32);
    for _ in 0..channels.max(1) {
        cv.push(raw);
    }
    p.ack(Command::SetSinkInputVolume(SetStreamVolumeParams { index, volume: cv }))
}

fn set_mute(p: &mut Pulse, sink: bool, index: u32, mute: bool) -> R<()> {
    let params = SetDeviceMuteParams { device_index: Some(index), device_name: None, mute };
    p.ack(if sink { Command::SetSinkMute(params) } else { Command::SetSourceMute(params) })
}

pub fn command(what: &str, args: &[SysValue]) -> R<()> {
    let mut p = Pulse::connect("pleamar")?;
    let (server, sinks, sources) = p.state()?;
    let sink = sinks.iter().find(|s| Some(&s.name) == server.default_sink_name.as_ref()).or(sinks.first());
    let source = sources.iter().filter(|s| s.monitor_of_sink_index.is_none()).find(|s| Some(&s.name) == server.default_source_name.as_ref());
    let out = |s: Option<&SinkInfo>| s.map(|s| (s.index, s.cvolume.channels().len(), level(&s.cvolume), s.muted)).ok_or("there is no output");
    let inp = |s: Option<&SourceInfo>| s.map(|s| (s.index, s.cvolume.channels().len(), level(&s.cvolume), s.muted)).ok_or("there is no input");
    match (what, args) {
        ("audio.volume", [SysValue::Num(v)]) => {
            let (i, ch, _, _) = out(sink)?;
            set_volume(&mut p, true, i, ch, *v)
        }
        // A step, as a fraction of one: 0.05 goes up, -0.05 goes down. Never above 100 %.
        ("audio.step", [SysValue::Num(d)]) => {
            let (i, ch, now, _) = out(sink)?;
            set_volume(&mut p, true, i, ch, now + d)
        }
        ("audio.mute", []) => {
            let (i, _, _, muted) = out(sink)?;
            set_mute(&mut p, true, i, !muted)
        }
        ("audio.mute", [SysValue::Bool(yes)]) => {
            let (i, _, _, _) = out(sink)?;
            set_mute(&mut p, true, i, *yes)
        }
        ("audio.input", [SysValue::Num(v)]) => {
            let (i, ch, _, _) = inp(source)?;
            set_volume(&mut p, false, i, ch, *v)
        }
        ("audio.input_mute", []) => {
            let (i, _, _, muted) = inp(source)?;
            set_mute(&mut p, false, i, !muted)
        }
        ("audio.input_mute", [SysValue::Bool(yes)]) => {
            let (i, _, _, _) = inp(source)?;
            set_mute(&mut p, false, i, *yes)
        }
        // One application: the `id` that `apps` gives.
        ("audio.app_volume", [SysValue::Num(id), SysValue::Num(v)]) => {
            let a = p.apps().into_iter().find(|a| a.index == *id as u32).ok_or(format!("there is no stream {id}"))?;
            set_app_volume(&mut p, a.index, a.cvolume.channels().len(), *v)
        }
        ("audio.app_mute", [SysValue::Num(id), rest @ ..]) if rest.len() <= 1 => {
            let a = p.apps().into_iter().find(|a| a.index == *id as u32).ok_or(format!("there is no stream {id}"))?;
            let mute = match rest {
                [SysValue::Bool(yes)] => *yes,
                _ => !a.muted,
            };
            p.ack(Command::SetSinkInputMute(SetStreamMuteParams { index: a.index, mute }))
        }
        // Where it plays or listens: the number the lists give.
        ("audio.default", [SysValue::Num(id)]) => {
            let id = *id as u32;
            if let Some(s) = sinks.iter().find(|s| s.index == id) {
                return p.ack(Command::SetDefaultSink(s.name.clone()));
            }
            if let Some(s) = sources.iter().find(|s| s.index == id) {
                return p.ack(Command::SetDefaultSource(s.name.clone()));
            }
            Err(format!("there is no device {id}"))
        }
        _ => Err(format!("'{what}' is not asked like that: audio.volume(0..1), audio.step(±0.05), audio.mute([true|false]), audio.input(0..1), audio.input_mute([true|false]), audio.default(id), audio.app_volume(id, 0..1), audio.app_mute(id[, true|false])")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A connection to nobody's server: what is written goes to the other end, which the test holds.
    fn silent_server() -> (Pulse, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        (Pulse { sock: BufReader::new(a), version: protocol::MAX_VERSION, seq: 1 }, b)
    }

    /// pleamar#164: a server that never answers the stream is waited for a while, not for ever.
    #[test]
    fn a_server_that_says_nothing_is_a_miss_after_the_wait() {
        let (mut p, _server) = silent_server();
        let t = Instant::now();
        let got = p.ask_within::<protocol::ServerInfo>(Command::GetServerInfo, Duration::from_millis(200)).unwrap();
        assert!(got.is_none(), "no answer is None");
        assert!(t.elapsed() >= Duration::from_millis(180) && t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// A server that hangs up is an error at once (the loop starts again after its pause), not a miss.
    #[test]
    fn a_server_that_hangs_up_is_an_error_not_a_miss() {
        let (mut p, server) = silent_server();
        drop(server);
        let t = Instant::now();
        assert!(p.ask_within::<protocol::ServerInfo>(Command::GetServerInfo, Duration::from_secs(10)).is_err());
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn a_packet_of_readings_gives_its_loudest() {
        let bytes: Vec<u8> = [0.1f32, -0.7, 0.4].iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(peak_of(&bytes), 0.7);
        assert_eq!(peak_of(&[]), 0.0);
        // A torn sample is left out, and nothing is louder than full scale.
        assert_eq!(peak_of(&[0, 0]), 0.0);
        assert_eq!(peak_of(&2.5f32.to_le_bytes()), 1.0);
    }

    #[test]
    fn the_meter_tells_a_burst_as_its_loudest_and_a_silence_once() {
        let mut m = Meter::new();
        let t = Instant::now();
        assert_eq!(m.feed(0.42, t), Some(0.42));
        // A burst, within the same thirtieth of a second: kept, and told with the next.
        assert_eq!(m.feed(0.9, t + Duration::from_millis(10)), None);
        assert_eq!(m.feed(0.2, t + Duration::from_millis(40)), Some(0.9));
        // A reading a cycle early (21 ms after the last) is not held back.
        assert_eq!(m.feed(0.3, t + Duration::from_millis(61)), Some(0.3));
        assert_eq!(m.feed(0.6, t + Duration::from_millis(66)), None);
        assert_eq!(m.feed(0.2, t + Duration::from_millis(70)), None);
        assert_eq!(m.feed(0.0, t + Duration::from_millis(82)), Some(0.6));
        assert_eq!(m.feed(0.0, t + Duration::from_millis(130)), Some(0.0));
        assert_eq!(m.feed(0.0, t + Duration::from_millis(160)), None);
        assert_eq!(m.feed(0.001, t + Duration::from_millis(190)), None);
        assert_eq!(m.feed(0.5, t + Duration::from_millis(220)), Some(0.5));
    }

    #[test]
    fn a_silence_is_told_at_once_and_drops_a_held_peak() {
        let mut m = Meter::new();
        let t = Instant::now();
        // At the start, before any reading: 0.
        assert_eq!(m.silence(), Some(0.0));
        assert_eq!(m.silence(), None);
        assert_eq!(m.feed(0.4, t), Some(0.4));
        // A louder reading held back by the cap...
        assert_eq!(m.feed(0.8, t + Duration::from_millis(5)), None);
        // ...is not told after a silence: the silence is told, now, inside the cap.
        assert_eq!(m.silence(), Some(0.0));
        assert_eq!(m.silence(), None);
        // Sound again is told at once, not held for the cap.
        assert_eq!(m.feed(0.3, t + Duration::from_millis(7)), Some(0.3));
        assert_eq!(m.feed(0.0, t + Duration::from_millis(40)), Some(0.0));
    }
}
