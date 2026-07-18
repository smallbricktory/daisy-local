//! Virtual capture sink lifecycle.
//!
//! On `VirtualSink::create(name)`, the engine asks `pactl` to load a
//! `module-null-sink` named `name` and a `module-loopback` that routes the
//! sink's monitor to the user's default output; audio routed into the new
//! sink continues to play through the actual speakers/headphones. Drop or
//! `dispose()` unloads both modules.

use crate::error::Result;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use crate::error::Error;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

pub struct VirtualSink {
    name: String,
    #[allow(dead_code)] // Read only on Linux; carried on all platforms.
    null_module_id: Option<u32>,
    #[allow(dead_code)]
    loopback_module_id: Option<u32>,
    disposed: bool,
}

impl VirtualSink {
    /// Load a daisy-managed null sink with the given name plus a loopback
    /// that routes the sink's monitor to the user's default output; audio
    /// routed into the new sink still plays through the user's speakers.
    pub fn create(name: &str) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            return Self::create_linux(name);
        }
        #[cfg(target_os = "windows")]
        {
            // WASAPI loopback captures the default render endpoint directly;
            // no virtual sink exists. An empty handle keeps upstream code
            // paths uniform; capture sites detect the Windows shape via
            // `monitor_source_name() == "wasapi-loopback"`.
            return Ok(Self {
                name: name.to_string(),
                null_module_id: None,
                loopback_module_id: None,
                disposed: false,
            });
        }
        #[cfg(target_os = "macos")]
        {
            // Core Audio system tap captures the system mix directly; no
            // routing sink exists. Empty handle like Windows; capture sites
            // resolve the system source via the "system-audio" sentinel.
            return Ok(Self {
                name: name.to_string(),
                null_module_id: None,
                loopback_module_id: None,
                disposed: false,
            });
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            let _ = name;
            Err(Error::NotSupported("virtual_sink::create"))
        }
    }

    #[cfg(target_os = "linux")]
    fn create_linux(name: &str) -> Result<Self> {
        // A sink with this name already existing is an orphan from a previous
        // run that did not shut down cleanly. Any modules matching the exact
        // shape are unloaded first. Only modules declaring `sink_name=<name>`
        // or `source=<name>.monitor` are touched; anything the user manually
        // loaded is left alone.
        if sink_exists(name)? {
            let orphans = find_orphan_modules(name)?;
            if !orphans.is_empty() {
                log::info!(
                    "sink '{}' exists from a previous run; unloading {} orphan module(s) before recreate",
                    name,
                    orphans.len(),
                );
                for id in orphans {
                    let _ = pactl_unload_module(id);
                }
            }
            // A sink name still owned by an unrelated process is an error.
            if sink_exists(name)? {
                return Err(Error::Subprocess(format!(
                    "sink {name} still exists after orphan cleanup; \
                     another process may own it. Run: \
                     pactl list short modules | grep {name}"
                )));
            }
        }

        let null_id = pactl_load_module(&[
            "module-null-sink",
            &format!("sink_name={name}"),
            "sink_properties=device.description=Daisy_Capture",
        ])?;

        // module-loopback: source = the null sink's monitor; sink omitted
        // (routes to the system's current default output). 20 ms latency.
        let loopback_id = match pactl_load_module(&[
            "module-loopback",
            &format!("source={name}.monitor"),
            "latency_msec=20",
        ]) {
            Ok(id) => id,
            Err(e) => {
                // Best-effort cleanup of the already-created null sink.
                let _ = pactl_unload_module(null_id);
                return Err(e);
            }
        };

        Ok(Self {
            name: name.to_string(),
            null_module_id: Some(null_id),
            loopback_module_id: Some(loopback_id),
            disposed: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the canonical monitor-source name for the created null sink.
    /// Pass this to `Source` lookups or directly to `capture_dual` as the
    /// system source.
    pub fn monitor_source_name(&self) -> String {
        #[cfg(target_os = "windows")]
        {
            // Sentinel — WASAPI loopback rides the default render endpoint, not
            // a per-sink monitor. Capture sites that see this string know to
            // resolve the system source via the WASAPI default-render path.
            return "wasapi-loopback".to_string();
        }
        #[cfg(target_os = "macos")]
        {
            // Sentinel — Core Audio tap captures the system mix; no per-sink monitor.
            return "system-audio".to_string();
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            format!("{}.monitor", self.name)
        }
    }

    /// Tear down both modules. Idempotent.
    pub fn dispose(&mut self) -> Result<()> {
        if self.disposed {
            return Ok(());
        }
        // Loopback unloads first; audio stops looping while the null sink is
        // dropped.
        #[cfg(target_os = "linux")]
        {
            if let Some(id) = self.loopback_module_id.take() {
                let _ = pactl_unload_module(id);
            }
            if let Some(id) = self.null_module_id.take() {
                let _ = pactl_unload_module(id);
            }
        }
        self.disposed = true;
        Ok(())
    }
}

impl Drop for VirtualSink {
    fn drop(&mut self) {
        let _ = self.dispose();
    }
}

#[cfg(target_os = "linux")]
fn pactl_load_module(args: &[&str]) -> Result<u32> {
    let mut cmd = Command::new("pactl");
    cmd.arg("load-module");
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().map_err(Error::Io)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Subprocess(format!(
            "pactl load-module failed: {stderr}"
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let id: u32 = stdout.trim().parse().map_err(|e| {
        Error::Subprocess(format!(
            "pactl load-module returned non-numeric id: {stdout:?} ({e})"
        ))
    })?;
    Ok(id)
}

#[cfg(target_os = "linux")]
fn pactl_unload_module(id: u32) -> Result<()> {
    let output = Command::new("pactl")
        .arg("unload-module")
        .arg(id.to_string())
        .stderr(Stdio::null())
        .output()
        .map_err(Error::Io)?;
    if !output.status.success() {
        return Err(Error::Subprocess(format!(
            "pactl unload-module {id} failed"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayingStream {
    pub id: u32,
    pub app_name: String,
    pub current_sink: String,
}

/// Parse the output of `wpctl status` for **output** stream entries (audio
/// being played to a sink). Returns an empty Vec on unparseable input.
///
/// Two on-the-wire shapes are accepted; PipeWire's format differs between
/// releases:
///
///   * Legacy (≤ 1.4.x): the sink rides on the stream header line in brackets
///       `105. Microsoft Edge                  [Built-in Speakers]`
///
///   * Current (1.4.7+, incl. 1.6.x): the header carries only id + app name,
///     and the destination sink is on the following indented port-link rows
///       `105. Microsoft Edge`
///       `        output_FR > Speaker:playback_FR`
///       `        output_FL > Speaker:playback_FL`
///     The sink node is the token left of `:` on the first link row; all
///     link rows for one stream point at the same sink and the first is
///     taken.
///
/// A stream whose link rows haven't appeared yet is held in `pending` and
/// flushed when the next header / blank line / section boundary arrives.
pub fn parse_wpctl_status_streams(text: &str) -> Vec<PlayingStream> {
    let mut out: Vec<PlayingStream> = Vec::new();
    let mut section: Section = Section::None;
    // A stream header seen in the new format whose sink we're still waiting to
    // read off an indented port-link row.
    let mut pending: Option<PlayingStream> = None;

    fn flush(pending: &mut Option<PlayingStream>, out: &mut Vec<PlayingStream>) {
        if let Some(s) = pending.take() {
            out.push(s);
        }
    }

    for raw in text.lines() {
        let line = raw.trim_start_matches([' ', '\t', '│', '├', '└', '─', '*']);
        let line = line.trim();

        // Section detection. wpctl prints "Output:" and "Input:" exactly once
        // each inside Streams.
        if line.starts_with("Output:") {
            flush(&mut pending, &mut out);
            section = Section::Output;
            continue;
        }
        if line.starts_with("Input:") {
            flush(&mut pending, &mut out);
            section = Section::Input;
            continue;
        }
        if line.is_empty() {
            // Blank line ends a section.
            flush(&mut pending, &mut out);
            section = Section::None;
            continue;
        }

        if section != Section::Output {
            continue;
        }

        // New-format port-link row for the stream we're holding:
        //   "output_FR > Speaker:playback_FR"  → sink node = "Speaker"
        // The '>' is the discriminator; header lines never contain one.
        if let Some(p) = pending.as_mut() {
            if p.current_sink.is_empty() {
                if let Some(sink) = parse_link_target_sink(line) {
                    p.current_sink = sink;
                    continue;
                }
            }
        }

        // Otherwise this should be a stream header: "ID. App Name[ [Sink]]".
        let dot = match line.find('.') {
            Some(i) => i,
            None => continue,
        };
        let id_str = line[..dot].trim();
        let id: u32 = match id_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // A new header closes out any stream still being assembled.
        flush(&mut pending, &mut out);

        let rest = line[dot + 1..].trim();
        // Legacy shape: sink in trailing brackets on the same line.
        if let Some(bracket_open) = rest.find('[') {
            if let Some(rel_close) = rest[bracket_open..].find(']') {
                let bracket_close = bracket_open + rel_close;
                out.push(PlayingStream {
                    id,
                    app_name: rest[..bracket_open].trim().to_string(),
                    current_sink: rest[bracket_open + 1..bracket_close].trim().to_string(),
                });
                continue;
            }
        }

        // Current shape: header only; sink arrives on the next link row(s).
        pending = Some(PlayingStream {
            id,
            app_name: rest.to_string(),
            current_sink: String::new(),
        });
    }

    flush(&mut pending, &mut out);
    out
}

/// Given an indented port-link row like `output_FR > Speaker:playback_FR`,
/// return the destination sink node name (`Speaker`). Returns `None` if the
/// row is not a link line (no `>`); stream-header lines fall through to the
/// header parser.
fn parse_link_target_sink(line: &str) -> Option<String> {
    let arrow = line.find('>')?;
    let target = line[arrow + 1..].trim();
    // target = "Speaker:playback_FR" — the node is everything before the port.
    let sink = target.split(':').next()?.trim();
    if sink.is_empty() {
        None
    } else {
        Some(sink.to_string())
    }
}

#[derive(PartialEq, Eq)]
enum Section {
    None,
    Output,
    Input,
}

#[cfg(target_os = "linux")]
impl VirtualSink {
    /// Move currently-playing output streams from their sinks into this
    /// virtual sink. Returns the list of (stream_id, original_sink_name)
    /// for the caller to restore on stop.
    pub fn route_playing_streams(&self) -> Result<Vec<(u32, String)>> {
        let status = std::process::Command::new("wpctl")
            .arg("status")
            .output()
            .map_err(Error::Io)?;
        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(Error::Subprocess(format!("wpctl status: {stderr}")));
        }
        let stdout = String::from_utf8_lossy(&status.stdout);
        let streams = parse_wpctl_status_streams(&stdout);

        let mut moved = Vec::new();
        for s in streams {
            // Skip streams already on this sink.
            if s.current_sink == self.name
                || s.current_sink == "daisy-capture"
                || s.current_sink == "Daisy_Capture"
            {
                continue;
            }
            // Skip the sink's own loopback module, which wpctl can list as a
            // stream routed to the user's real sink (app_name is empty).
            if s.app_name.is_empty() {
                continue;
            }
            let result = std::process::Command::new("wpctl")
                .args(["move", &s.id.to_string(), &self.name])
                .output()
                .map_err(Error::Io)?;
            if result.status.success() {
                log::info!(
                    "moved stream {} ({}) from {} to {}",
                    s.id,
                    s.app_name,
                    s.current_sink,
                    self.name
                );
                moved.push((s.id, s.current_sink));
            } else {
                let stderr = String::from_utf8_lossy(&result.stderr);
                log::warn!(
                    "could not move stream {} ({}): {}",
                    s.id,
                    s.app_name,
                    stderr
                );
            }
        }
        Ok(moved)
    }

    /// Restore previous routing for streams that were moved into this sink.
    /// Best-effort: silently skips streams whose IDs no longer exist or whose
    /// original sinks have disappeared.
    pub fn restore_routing(moved: &[(u32, String)]) {
        for (id, original_sink_name) in moved {
            // Look up the original sink's current PW ID by name in `wpctl status`.
            let status = match std::process::Command::new("wpctl").arg("status").output() {
                Ok(o) if o.status.success() => o,
                _ => continue,
            };
            let text = String::from_utf8_lossy(&status.stdout);
            let target_id = text.lines().find_map(|l| {
                let l = l.trim_start_matches([' ', '│', '├', '└', '─', '*']);
                let l = l.trim();
                let dot = l.find('.')?;
                let id_str = l[..dot].trim();
                let candidate_id: u32 = id_str.parse().ok()?;
                let rest = l[dot + 1..].trim();
                if rest.starts_with(original_sink_name.as_str()) {
                    Some(candidate_id)
                } else {
                    None
                }
            });
            if let Some(target) = target_id {
                let _ = std::process::Command::new("wpctl")
                    .args(["move", &id.to_string(), &target.to_string()])
                    .output();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl VirtualSink {
    /// Stub on non-Linux, where no virtual sink ever exists. Returns Ok with
    /// an empty list.
    pub fn route_playing_streams(&self) -> Result<Vec<(u32, String)>> {
        Ok(Vec::new())
    }

    pub fn restore_routing(_moved: &[(u32, String)]) {}
}

#[cfg(target_os = "linux")]
fn sink_exists(name: &str) -> Result<bool> {
    let output = Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
        .map_err(Error::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name)))
}

/// Find PipeWire/PulseAudio modules matching the daisy shape: a
/// `module-null-sink` with `sink_name=<name>`, or a `module-loopback` whose
/// source is the sink's monitor. Returns loopback module IDs before sink
/// module IDs.
#[cfg(target_os = "linux")]
fn find_orphan_modules(name: &str) -> Result<Vec<u32>> {
    let output = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
        .map_err(Error::Io)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Subprocess(format!(
            "pactl list short modules: {stderr}"
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sink_marker = format!("sink_name={name}");
    let source_marker = format!("source={name}.monitor");

    // Collected by kind; loopbacks unload before null-sinks.
    let mut sinks: Vec<u32> = Vec::new();
    let mut loopbacks: Vec<u32> = Vec::new();
    for line in stdout.lines() {
        // pactl list short uses tab separators: <id>\t<module>\t<args>
        let mut parts = line.split('\t');
        let Some(id_str) = parts.next() else { continue };
        let Some(module) = parts.next() else { continue };
        let args = parts.next().unwrap_or("");
        let Ok(id) = id_str.trim().parse::<u32>() else { continue };
        match module.trim() {
            "module-null-sink" if args.contains(&sink_marker) => sinks.push(id),
            "module-loopback" if args.contains(&source_marker) => loopbacks.push(id),
            _ => {}
        }
    }
    let mut out = loopbacks;
    out.extend(sinks);
    Ok(out)
}
