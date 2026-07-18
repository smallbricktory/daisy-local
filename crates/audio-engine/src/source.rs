//! PipeWire source enumeration.
//!
//! Lists capture-capable sources (mics, monitor-of-sink loopback sources)
//! with stable PipeWire object IDs and human-readable names.
//!
//! ## Monitor source representation
//!
//! On typical PipeWire setups, monitor sources are not separate PW registry
//! nodes. PipeWire exposes sinks as `Audio/Sink` nodes; the PulseAudio
//! compatibility layer (and `pactl list sources`) synthesises a virtual
//! `.monitor` source for each sink. This module replicates that convention:
//! every `Audio/Sink` node is returned as a `SourceKind::Monitor` entry whose
//! `node_name` is `<sink_node_name>.monitor` — the name `pw-stream` and
//! `pactl` accept as a capture target.

use crate::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// A real input — typically a microphone.
    Mic,
    /// A monitor source — captures what is playing through a sink.
    Monitor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// PipeWire object ID, stable for the lifetime of the PW session.
    pub id: u32,
    /// The PipeWire `node.name` (e.g. `alsa_input.pci-…`).
    /// For monitor sources this is `<sink_node_name>.monitor`.
    pub node_name: String,
    /// Human-friendly description; falls back to `node_name` if PW didn't provide one.
    pub description: String,
    pub kind: SourceKind,
    pub default_sample_rate: u32,
    pub default_channels: u16,
}

/// One-shot enumeration: starts a PipeWire main loop, walks the registry,
/// returns all capture-capable sources.
///
/// Returns `Err` if PipeWire is unavailable or enumeration fails.
/// Never returns `Ok(empty)` on a system with a running PipeWire daemon that
/// has at least one audio device — an empty `Vec` is only valid on headless CI.
pub fn list_sources() -> Result<Vec<Source>> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::list_sources_blocking()
    }
    #[cfg(target_os = "windows")]
    {
        crate::wasapi::list_sources_blocking()
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::list_sources_blocking()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err(crate::error::Error::NotSupported("list_sources"))
    }
}

#[cfg(target_os = "linux")]
mod pipewire_impl {
    use super::{Source, SourceKind};
    use crate::error::{Error, Result};
    use pipewire as pw;
    use pw::core::PW_ID_CORE;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    pub(super) fn list_sources_blocking() -> Result<Vec<Source>> {
        // pipewire::init() is idempotent; MainLoop::new() also calls it.
        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;

        let context = pw::context::Context::new(&mainloop)
            .map_err(|e| Error::PipeWire(format!("Context::new: {e}")))?;

        let core = context
            .connect(None)
            .map_err(|e| Error::PipeWire(format!("Context::connect: {e}")))?;

        let registry = core
            .get_registry()
            .map_err(|e| Error::PipeWire(format!("Core::get_registry: {e}")))?;

        // Accumulate raw entries from the registry callback. Both
        // Audio/Source and Audio/Sink nodes are collected; sinks become
        // monitor entries.
        let sources: Rc<RefCell<Vec<Source>>> = Rc::new(RefCell::new(Vec::new()));
        let sources_for_listener = Rc::clone(&sources);

        // The global listener registers before sync() is called; no events
        // are missed.
        let _listener_reg = registry
            .add_listener_local()
            .global(move |global| {
                let Some(props) = global.props else {
                    return;
                };

                let media_class = props.get("media.class").unwrap_or("");

                let kind = match media_class {
                    "Audio/Source" => Some(SourceKind::Mic),
                    "Audio/Sink" => Some(SourceKind::Monitor),
                    _ => None,
                };

                let Some(kind) = kind else {
                    return;
                };

                let id = global.id;

                let raw_node_name = props.get("node.name").unwrap_or("").to_string();

                // Monitor sources are addressed as "<sink_name>.monitor" in PipeWire.
                let node_name = match kind {
                    SourceKind::Monitor => format!("{raw_node_name}.monitor"),
                    SourceKind::Mic => raw_node_name.clone(),
                };

                let raw_description = props
                    .get("node.description")
                    .or_else(|| props.get("node.nick"))
                    .unwrap_or(if raw_node_name.is_empty() {
                        ""
                    } else {
                        raw_node_name.as_str()
                    })
                    .to_string();

                let description = match kind {
                    SourceKind::Monitor => format!("Monitor of {raw_description}"),
                    SourceKind::Mic => raw_description,
                };

                let sr = props
                    .get("audio.rate")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(48_000);

                let ch = props
                    .get("audio.channels")
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(2);

                sources_for_listener.borrow_mut().push(Source {
                    id,
                    node_name,
                    description,
                    kind,
                    default_sample_rate: sr,
                    default_channels: ch,
                });
            })
            .register();

        // Send a sync round-trip.  The `done` event fires after the server has
        // processed all pending events (including all `global` callbacks above).
        let done = Rc::new(Cell::new(false));
        let done_clone = done.clone();
        let loop_clone = mainloop.downgrade();

        let pending = core
            .sync(0)
            .map_err(|e| Error::PipeWire(format!("Core::sync: {e}")))?;

        let _listener_core = core
            .add_listener_local()
            .done(move |id, seq| {
                if id == PW_ID_CORE && seq == pending {
                    done_clone.set(true);
                    if let Some(ml) = loop_clone.upgrade() {
                        ml.quit();
                    }
                }
            })
            .register();

        // Run until the `done` callback fires and calls quit().
        // Context::connect() has already succeeded here; a live daemon
        // delivers the `done` event.
        while !done.get() {
            mainloop.run();
        }

        // Drop the listeners explicitly before attempting Rc::try_unwrap.
        // The registry listener closure owns `sources_for_listener` (an Rc clone).
        // Dropping it here ensures the only remaining Rc is `sources` itself.
        drop(_listener_reg);
        drop(_listener_core);

        let sources = Rc::try_unwrap(sources)
            .map_err(|_| Error::PipeWire("registry listener still holds Rc".into()))?
            .into_inner();

        Ok(sources)
    }
}
