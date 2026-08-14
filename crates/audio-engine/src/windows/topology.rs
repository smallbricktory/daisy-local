//! Windows topology watcher: an `IMMNotificationClient` that turns MMDevice
//! notifications into [`TopologyEvent`]s.

use crate::source::SourceKind;
use crate::topology::{DeviceKey, TopologyEvent};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use windows_core::{implement, Interface, PCWSTR};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, ERole, IMMDevice, IMMEndpoint, IMMNotificationClient,
    IMMNotificationClient_Impl, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::Foundation::PROPERTYKEY;

// ── Endpoint-volume watch (VolumeChanged emitter) ──────────────────────────

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeWatch {
    key: DeviceKey,
    tx: Mutex<Sender<TopologyEvent>>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeWatch_Impl {
    fn OnNotify(&self, pnotify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows_core::Result<()> {
        if pnotify.is_null() {
            return Ok(());
        }
        let d = unsafe { &*pnotify };
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(TopologyEvent::VolumeChanged {
                key: self.key.clone(),
                volume: d.fMasterVolume,
                muted: d.bMuted.as_bool(),
            });
        }
        Ok(())
    }
}

/// One registered endpoint-volume callback; unregistered on drop.
struct VolumeReg {
    vol: IAudioEndpointVolume,
    cb: IAudioEndpointVolumeCallback,
}

impl Drop for VolumeReg {
    fn drop(&mut self) {
        unsafe {
            let _ = self.vol.UnregisterControlChangeNotify(&self.cb);
        }
    }
}

type VolumeRegs = Arc<Mutex<HashMap<String, VolumeReg>>>;

fn register_volume_watch(
    device: &IMMDevice,
    id_str: &str,
    tx: Sender<TopologyEvent>,
    regs: &VolumeRegs,
) {
    let Ok(vol) = (unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }) else {
        return;
    };
    let cb: IAudioEndpointVolumeCallback =
        VolumeWatch { key: DeviceKey(id_str.to_string()), tx: Mutex::new(tx) }.into();
    if unsafe { vol.RegisterControlChangeNotify(&cb) }.is_ok() {
        regs.lock()
            .unwrap()
            .insert(id_str.to_string(), VolumeReg { vol, cb });
    }
}

/// Register a volume watch on every ACTIVE capture endpoint.
fn register_all_capture_volumes(tx: &Sender<TopologyEvent>, regs: &VolumeRegs) {
    let Ok(enumerator) = super::create_enumerator() else { return };
    let Ok(coll) = (unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }) else {
        return;
    };
    let n = unsafe { coll.GetCount() }.unwrap_or(0);
    for i in 0..n {
        let Ok(device) = (unsafe { coll.Item(i) }) else { continue };
        let Some(id_str) = super::source::read_device_id(&device) else { continue };
        register_volume_watch(&device, &id_str, tx.clone(), regs);
    }
}

/// Option-adapter over the crate's `pcwstr_to_string` (empty → `None`).
fn pcwstr_string(id: &PCWSTR) -> Option<String> {
    let s = super::pcwstr_to_string(*id);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[implement(IMMNotificationClient)]
struct NotifyClient {
    tx: Mutex<Sender<TopologyEvent>>,
    /// Endpoint-volume registrations, keyed by device id string; hotplugged
    /// capture devices are registered here, removals unregister.
    volume_regs: VolumeRegs,
}

impl NotifyClient {
    fn send(&self, ev: TopologyEvent) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(ev);
        }
    }

    /// Mic or Monitor (plus the device) from the endpoint's data-flow.
    fn kind_of(id_str: &str) -> Option<(SourceKind, IMMDevice)> {
        let enumerator = super::create_enumerator().ok()?;
        let wide: Vec<u16> = id_str.encode_utf16().chain(std::iter::once(0)).collect();
        let device = unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }.ok()?;
        let endpoint: IMMEndpoint = device.cast().ok()?;
        let flow = unsafe { endpoint.GetDataFlow() }.ok()?;
        let kind = if flow == eCapture {
            SourceKind::Mic
        } else {
            SourceKind::Monitor
        };
        Some((kind, device))
    }
}

impl IMMNotificationClient_Impl for NotifyClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        pwstrdeviceid: &PCWSTR,
        dwnewstate: DEVICE_STATE,
    ) -> windows_core::Result<()> {
        let Some(id_str) = pcwstr_string(pwstrdeviceid) else {
            return Ok(());
        };
        if dwnewstate == DEVICE_STATE_ACTIVE {
            let Some((kind, device)) = NotifyClient::kind_of(&id_str) else {
                return Ok(());
            };
            if kind == SourceKind::Mic {
                if let Ok(tx) = self.tx.lock() {
                    register_volume_watch(&device, &id_str, tx.clone(), &self.volume_regs);
                }
            }
            let key = match kind {
                SourceKind::Mic => DeviceKey(id_str),
                SourceKind::Monitor => DeviceKey::monitor_of(&id_str),
            };
            self.send(TopologyEvent::DeviceAdded { key, kind });
        } else {
            self.volume_regs.lock().unwrap().remove(&id_str);
            // The device is gone; its flow direction is no longer queryable.
            // Emit removal under both key shapes — the snapshot ignores the
            // one it never contained.
            self.send(TopologyEvent::DeviceRemoved {
                key: DeviceKey::monitor_of(&id_str),
            });
            self.send(TopologyEvent::DeviceRemoved {
                key: DeviceKey(id_str),
            });
        }
        Ok(())
    }

    fn OnDeviceAdded(&self, _pwstrdeviceid: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _pwstrdeviceid: &PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        pwstrdefaultdeviceid: &PCWSTR,
    ) -> windows_core::Result<()> {
        if role != eConsole {
            return Ok(());
        }
        let id = pcwstr_string(pwstrdefaultdeviceid);
        if flow == eCapture {
            self.send(TopologyEvent::DefaultInputChanged {
                key: id.map(DeviceKey),
            });
        } else if flow == eRender {
            let class = super::default_output_class()
                .unwrap_or(crate::routing::OutputClass::Unknown);
            self.send(TopologyEvent::DefaultOutputChanged {
                key: id.map(|s| DeviceKey::monitor_of(&s)),
                class,
            });
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows_core::Result<()> {
        Ok(())
    }
}

pub(crate) struct WatcherHandle {
    quit: Option<Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub(crate) fn spawn_watcher(tx: Sender<TopologyEvent>) -> WatcherHandle {
    let (quit_tx, quit_rx) = std::sync::mpsc::channel::<()>();
    let join = std::thread::Builder::new()
        .name("daisy-topology-watch".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            match super::create_enumerator() {
                Ok(enumerator) => {
                    let volume_regs: VolumeRegs = Arc::new(Mutex::new(HashMap::new()));
                    register_all_capture_volumes(&tx, &volume_regs);
                    let client: IMMNotificationClient = NotifyClient {
                        tx: Mutex::new(tx),
                        volume_regs: Arc::clone(&volume_regs),
                    }
                    .into();
                    let registered = unsafe {
                        enumerator.RegisterEndpointNotificationCallback(&client)
                    };
                    match registered {
                        Ok(()) => {
                            // Block until told to quit; notifications arrive on
                            // COM threads independent of this one.
                            let _ = quit_rx.recv();
                            unsafe {
                                let _ = enumerator
                                    .UnregisterEndpointNotificationCallback(&client);
                            }
                            // Unregister volume callbacks before COM teardown.
                            volume_regs.lock().unwrap().clear();
                        }
                        Err(e) => {
                            log::warn!("topology watcher: register notifications: {e}")
                        }
                    }
                }
                Err(e) => log::warn!("topology watcher: create enumerator: {e}"),
            }
            unsafe { CoUninitialize() };
        })
        .expect("spawn topology watcher thread");
    WatcherHandle {
        quit: Some(quit_tx),
        join: Some(join),
    }
}

/// Default capture endpoint's stable key (its IMMDevice id string).
pub(crate) fn default_input_key() -> Option<DeviceKey> {
    // Guard (not a bare CoInitializeEx): this runs on callers' threads, and
    // an unbalanced init would leak a COM refcount per call.
    let _com = super::ComGuard::init().ok()?;
    let enumerator = super::create_enumerator().ok()?;
    let device = unsafe {
        enumerator.GetDefaultAudioEndpoint(
            eCapture,
            windows::Win32::Media::Audio::eConsole,
        )
    }
    .ok()?;
    super::source::read_device_id(&device).map(DeviceKey)
}
