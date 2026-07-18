import Foundation
import CoreAudio
import AudioToolbox
import AVFoundation

// track: 0 = system audio, 1 = microphone
public typealias PcmCallback = @convention(c) (UnsafeMutableRawPointer?, Int32, UnsafePointer<Float>?, Int32, Int32, Int32) -> Void
public typealias StateCallback = @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
// Mic level meter: ctx + peak amplitude (0..1).
public typealias RmsCallback = @convention(c) (UnsafeMutableRawPointer?, Float) -> Void

let DAISY_STATE_RUNNING: Int32 = 0
let DAISY_STATE_STOPPED: Int32 = 1
let DAISY_STATE_ERROR: Int32 = 2
let DAISY_STATE_PERM_DENIED: Int32 = 3

let DAISY_TRACK_SYSTEM: Int32 = 0
let DAISY_TRACK_MIC: Int32 = 1

// ── Log bridge (Swift → Rust) ───────────────────────────────────────────────
// The shim has no logger of its own; Swift print()/NSLog never reach Daisy's
// fern log file. So Rust hands us a callback that forwards into `log::*` tagged
// "macos-shim:". Level: 0 info · 1 warn · 2 error. Every CoreAudio/AVAudioEngine
// error path below calls daisyLog so failures leave a trail instead of vanishing
// into a bare return code.
public typealias LogCallback = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void
var g_log: LogCallback?

@_cdecl("daisy_set_log_callback")
public func daisy_set_log_callback(_ cb: @escaping LogCallback) {
    g_log = cb
    daisyLog("log bridge installed")
}

func daisyLog(_ msg: String, _ level: Int32 = 0) {
    guard let cb = g_log else { return }
    msg.withCString { cb(level, $0) }
}
func daisyWarn(_ msg: String) { daisyLog(msg, 1) }
func daisyErr(_ msg: String) { daisyLog(msg, 2) }

// Human-readable OSStatus: render the 4-char code when printable (CoreAudio
// fourcc errors like 'who?'/'!obj'), else the raw integer.
func osStatusString(_ st: OSStatus) -> String {
    let n = UInt32(bitPattern: st)
    let bytes = [UInt8(n >> 24 & 0xff), UInt8(n >> 16 & 0xff), UInt8(n >> 8 & 0xff), UInt8(n & 0xff)]
    if bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7f }) {
        return "\(st) '\(String(bytes: bytes, encoding: .ascii) ?? "?")'"
    }
    return "\(st)"
}

/// Human-readable name for a CoreAudio device id (the AudioObjectIDs in the log
/// are ephemeral HAL handles macOS shows nowhere — names make the log legible,
/// e.g. spotting a virtual/Krisp/Teams mic Daisy switched to). "?" if it can't
/// resolve (stale/absent id).
func deviceName(_ id: AudioDeviceID) -> String {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioObjectPropertyName,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var cf: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    let st = AudioObjectGetPropertyData(id, &addr, 0, nil, &size, &cf)
    guard st == noErr, let name = cf?.takeRetainedValue() else { return "?" }
    return name as String
}

// ── Microphone capture (AVAudioEngine input tap) ───────────────────────────
// One class serves both recording (emit PCM via onPcm) and the live level bar
// (emit peak RMS via onRms). Mic-only: needs just the microphone permission.

final class DaisyMicEngine {
    let engine = AVAudioEngine()
    let ctx: UnsafeMutableRawPointer?
    let onPcm: PcmCallback?
    let onRms: RmsCallback?

    init(ctx: UnsafeMutableRawPointer?, onPcm: PcmCallback?, onRms: RmsCallback?) {
        self.ctx = ctx; self.onPcm = onPcm; self.onRms = onRms
    }

    func start(deviceID: UInt32) -> Int32 {
        // Gate on microphone authorization FIRST. If we install the tap + start
        // the engine before the user has granted mic access, AVAudioEngine runs
        // silent (delivers 0 buffers) and does NOT recover when the grant lands
        // mid-run — the mic track comes out empty on the first record and only
        // works on the second. Request + block until the user answers (we run
        // on a Rust worker thread, never the main thread, so waiting is safe).
        let mode = onPcm != nil ? "capture" : "meter"
        let auth = AVCaptureDevice.authorizationStatus(for: .audio)
        daisyLog("mic engine start: deviceID=\(deviceID) (0=system-default) mode=\(mode) micAuth=\(auth.rawValue) (0=notDetermined 1=restricted 2=denied 3=authorized)")
        switch auth {
        case .authorized:
            break
        case .notDetermined:
            daisyLog("mic engine: requesting mic authorization (blocking until user answers)")
            let sem = DispatchSemaphore(value: 0)
            AVCaptureDevice.requestAccess(for: .audio) { _ in sem.signal() }
            sem.wait()
            let granted = AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
            daisyLog("mic engine: mic authorization \(granted ? "GRANTED" : "DENIED") by user")
            if !granted { daisyErr("mic engine start FAILED: mic permission denied (rc=-4)"); return -4 }
        case .denied, .restricted:
            daisyErr("mic engine start FAILED: mic permission denied/restricted (rc=-4)")
            return -4
        @unknown default:
            daisyErr("mic engine start FAILED: unknown mic auth status \(auth.rawValue) (rc=-4)")
            return -4
        }
        let input = engine.inputNode
        // Point the input HAL unit at the chosen CoreAudio device (id from source
        // enumeration). deviceID == 0 means "system default input".
        if deviceID != 0, let au = input.audioUnit {
            var dev = AudioDeviceID(deviceID)
            let st = AudioUnitSetProperty(
                au, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global,
                0, &dev, UInt32(MemoryLayout<AudioDeviceID>.size))
            if st != noErr {
                daisyErr("mic engine start FAILED: AudioUnitSetProperty(CurrentDevice=\(deviceID)) rc=\(osStatusString(st)) — device likely stale/absent/aggregate")
                return st
            }
            daisyLog("mic engine: HAL input device set to \(deviceID) (\(deviceName(AudioDeviceID(deviceID))))")
        }
        let fmt = input.inputFormat(forBus: 0)
        // NOTE: this format is negotiated against the device the inputNode first
        // materialized on (the system default). After an in-place device switch
        // it can be STALE — e.g. built-in mic requested but reports 24kHz (AirPods
        // HFP rate) instead of 48kHz. Logged so the mismatch is visible.
        daisyLog("mic engine: negotiated inputFormat sr=\(Int(fmt.sampleRate)) ch=\(fmt.channelCount) for deviceID=\(deviceID)")
        if fmt.channelCount == 0 {
            daisyErr("mic engine start FAILED: inputFormat reports 0 channels for deviceID=\(deviceID) (rc=-2) — device has no input or wrong device")
            return -2
        }
        let sr = Int32(fmt.sampleRate)
        let ctxLocal = self.ctx
        let onPcm = self.onPcm
        let onRms = self.onRms
        input.installTap(onBus: 0, bufferSize: 1024, format: fmt) { buf, _ in
            guard let ch = buf.floatChannelData else { return }
            let n = Int(buf.frameLength)
            if n == 0 { return }
            let data = ch[0]   // channel 0 as mono
            if let onPcm = onPcm {
                onPcm(ctxLocal, DAISY_TRACK_MIC, data, Int32(n), 1, sr)
            }
            if let onRms = onRms {
                var peak: Float = 0
                for i in 0..<n { peak = max(peak, abs(data[i])) }
                onRms(ctxLocal, peak)
            }
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            let ns = error as NSError
            daisyErr("mic engine start FAILED: AVAudioEngine.start() threw — \(ns.domain) code=\(ns.code): \(ns.localizedDescription) (rc=-1) — input device likely contended (another engine/app holds it)")
            return -1
        }
        daisyLog("mic engine STARTED: deviceID=\(deviceID) mode=\(mode) sr=\(sr)")
        return 0
    }

    func stop() {
        daisyLog("mic engine stop")
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }
}

// ── System-audio capture (Core Audio process tap → aggregate → IOProc) ─────
// Global tap of all process output, excluding our own process. Uses only the
// microphone permission (validated on hardware), never ScreenCaptureKit.

final class DaisySystemTap {
    let ctx: UnsafeMutableRawPointer?
    let onPcm: PcmCallback
    var tapID = AudioObjectID(kAudioObjectUnknown)
    var aggID = AudioObjectID(kAudioObjectUnknown)
    var ioProcID: AudioDeviceIOProcID?

    init(ctx: UnsafeMutableRawPointer?, onPcm: @escaping PcmCallback) {
        self.ctx = ctx; self.onPcm = onPcm
    }

    func start() -> Int32 {
        // Exclude our own process so we never record Daisy's own playback /
        // notification sounds. If PID translation fails, fall back to a global
        // tap (empty exclude list).
        daisyLog("system tap start: creating Core Audio process tap (global, excluding self)")
        var excluded: [AudioObjectID] = []
        if let obj = Self.audioObject(forPID: ProcessInfo.processInfo.processIdentifier) {
            excluded = [obj]
        } else {
            daisyWarn("system tap: PID→AudioObject translation failed; using GLOBAL tap (will also capture Daisy's own output)")
        }
        let desc = CATapDescription(monoGlobalTapButExcludeProcesses: excluded)
        desc.muteBehavior = .unmuted

        var st = AudioHardwareCreateProcessTap(desc, &tapID)
        if st != noErr {
            daisyErr("system tap FAILED: AudioHardwareCreateProcessTap rc=\(osStatusString(st)) — system-audio TCC (NSAudioCaptureUsageDescription) likely denied/not-granted")
            return st
        }
        daisyLog("system tap: process tap created (tapID=\(tapID))")

        // Tap UID (CFString) to reference it from the aggregate device.
        var uidAddr = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var tapUID: CFString = "" as CFString
        var uidSize = UInt32(MemoryLayout<CFString>.size)
        st = withUnsafeMutablePointer(to: &tapUID) {
            AudioObjectGetPropertyData(tapID, &uidAddr, 0, nil, &uidSize, $0)
        }
        if st != noErr {
            daisyErr("system tap FAILED: read tap UID rc=\(osStatusString(st))")
            return st
        }

        // UNIQUE per-instance UID. A fixed UID collided across back-to-back
        // sessions: if the previous tap's aggregate hadn't finished tearing down
        // in coreaudiod, creating a new device with the same UID forced a
        // reconciliation that delayed the IOProc (part of the 140s system-tap
        // stall). A fresh UUID each start sidesteps that entirely.
        let aggUID = "ai.daisy.systemtap.agg." + UUID().uuidString
        let aggDesc: [String: Any] = [
            kAudioAggregateDeviceNameKey: "DaisySystemTap",
            kAudioAggregateDeviceUIDKey: aggUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[String: Any]](),
            kAudioAggregateDeviceTapListKey: [
                [ kAudioSubTapDriftCompensationKey: true,
                  kAudioSubTapUIDKey: tapUID ]
            ],
        ]
        st = AudioHardwareCreateAggregateDevice(aggDesc as CFDictionary, &aggID)
        if st != noErr {
            daisyErr("system tap FAILED: AudioHardwareCreateAggregateDevice rc=\(osStatusString(st))")
            return st
        }
        daisyLog("system tap: aggregate device created (aggID=\(aggID), uid=\(aggUID))")

        var fmtAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: 0)
        var asbd = AudioStreamBasicDescription()
        var asbdSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        st = AudioObjectGetPropertyData(aggID, &fmtAddr, 0, nil, &asbdSize, &asbd)
        if st != noErr {
            daisyErr("system tap FAILED: read aggregate stream format rc=\(osStatusString(st))")
            return st
        }
        daisyLog("system tap: aggregate format sr=\(Int(asbd.mSampleRate)) ch=\(asbd.mChannelsPerFrame) bits=\(asbd.mBitsPerChannel) flags=\(asbd.mFormatFlags)")
        // Deterministic format gate: only 32-bit float PCM is interpreted.
        guard (asbd.mFormatFlags & kAudioFormatFlagIsFloat) != 0,
              asbd.mBitsPerChannel == 32 else {
            daisyErr("system tap FAILED: aggregate format not 32-bit float (bits=\(asbd.mBitsPerChannel) flags=\(asbd.mFormatFlags)) (rc=-3)")
            return -3
        }
        let ch = Int32(max(asbd.mChannelsPerFrame, 1))
        let sr = Int32(asbd.mSampleRate)
        let ctxLocal = self.ctx
        let cb = self.onPcm

        st = AudioDeviceCreateIOProcIDWithBlock(&ioProcID, aggID, nil) {
            (_, inInputData, _, _, _) in
            let abl = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInputData))
            guard let buf = abl.first, let mData = buf.mData else { return }
            let floatsInBuf0 = Int32(buf.mDataByteSize) / 4
            let floats = mData.assumingMemoryBound(to: Float.self)
            if abl.count > 1 {
                // Planar: each AudioBuffer is one channel. Forward channel 0.
                cb(ctxLocal, DAISY_TRACK_SYSTEM, floats, floatsInBuf0, 1, sr)
            } else {
                // Interleaved (or mono): buffer 0 holds frames*channels floats.
                cb(ctxLocal, DAISY_TRACK_SYSTEM, floats, floatsInBuf0 / ch, ch, sr)
            }
        }
        if st != noErr {
            daisyErr("system tap FAILED: AudioDeviceCreateIOProcIDWithBlock rc=\(osStatusString(st))")
            return st
        }
        let startRc = AudioDeviceStart(aggID, ioProcID)
        if startRc != noErr {
            daisyErr("system tap FAILED: AudioDeviceStart rc=\(osStatusString(startRc))")
        } else {
            daisyLog("system tap STARTED: IOProc running on aggID=\(aggID) sr=\(sr) ch=\(ch)")
        }
        return startRc
    }

    func stop() {
        if let p = ioProcID {
            AudioDeviceStop(aggID, p)
            AudioDeviceDestroyIOProcID(aggID, p)
            ioProcID = nil
        }
        if aggID != kAudioObjectUnknown {
            AudioHardwareDestroyAggregateDevice(aggID)
            aggID = kAudioObjectUnknown
        }
        if tapID != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = kAudioObjectUnknown
        }
    }

    static func audioObject(forPID pid: pid_t) -> AudioObjectID? {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var inPid = pid
        var obj = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let st = withUnsafeMutablePointer(to: &inPid) { pidPtr -> OSStatus in
            AudioObjectGetPropertyData(
                AudioObjectID(kAudioObjectSystemObject), &addr,
                UInt32(MemoryLayout<pid_t>.size), pidPtr, &size, &obj)
        }
        if st != noErr || obj == kAudioObjectUnknown { return nil }
        return obj
    }
}

// ── Capture coordinator ────────────────────────────────────────────────────
// All-or-nothing: a system-tap failure fails the whole session (no mic-only
// fallback). Synchronous start — onState fires before daisy_capture_start
// returns.

final class DaisyCapture {
    let system: DaisySystemTap
    var mic: DaisyMicEngine?
    let micDeviceID: UInt32
    let ctx: UnsafeMutableRawPointer?
    let onState: StateCallback

    init(ctx: UnsafeMutableRawPointer?, onPcm: @escaping PcmCallback,
         onState: @escaping StateCallback, wantMic: Bool, micDeviceID: UInt32) {
        self.ctx = ctx
        self.onState = onState
        self.micDeviceID = micDeviceID
        self.system = DaisySystemTap(ctx: ctx, onPcm: onPcm)
        if wantMic { self.mic = DaisyMicEngine(ctx: ctx, onPcm: onPcm, onRms: nil) }
    }

    func start() -> Int32 {
        daisyLog("capture coordinator start: wantMic=\(mic != nil) micDeviceID=\(micDeviceID)")
        let st = system.start()
        if st != noErr {
            daisyErr("capture coordinator: system tap failed (rc=\(osStatusString(st))) — whole session fails (all-or-nothing)")
            onState(ctx, DAISY_STATE_ERROR)
            return st
        }
        if let mic = mic {
            let mst = mic.start(deviceID: micDeviceID)
            if mst != noErr {
                // -2 = no input channels, -4 = mic authorization denied: both are
                // "we can't get the mic" → surface as a permission problem.
                let denied = (mst == -2 || mst == -4)
                daisyErr("capture coordinator: mic failed (rc=\(mst)) → \(denied ? "PERM_DENIED" : "ERROR"); tearing down system tap")
                system.stop()
                onState(ctx, denied ? DAISY_STATE_PERM_DENIED : DAISY_STATE_ERROR)
                return mst
            }
        }
        daisyLog("capture coordinator STARTED: state=RUNNING")
        onState(ctx, DAISY_STATE_RUNNING)
        return 0
    }

    func stop() {
        mic?.stop()
        system.stop()
        onState(ctx, DAISY_STATE_STOPPED)
    }
}

// ── C-ABI entry points ─────────────────────────────────────────────────────

var g_capture: DaisyCapture?

@_cdecl("daisy_capture_start")
public func daisy_capture_start(
    _ wantMic: Int32,
    _ micDeviceID: UInt32,
    _ ctx: UnsafeMutableRawPointer?,
    _ onPcm: @escaping PcmCallback,
    _ onState: @escaping StateCallback
) -> Int32 {
    daisyLog("daisy_capture_start(wantMic=\(wantMic), micDeviceID=\(micDeviceID))")
    let cap = DaisyCapture(ctx: ctx, onPcm: onPcm, onState: onState,
                           wantMic: wantMic != 0, micDeviceID: micDeviceID)
    g_capture = cap
    let rc = cap.start()
    if rc != 0 {
        daisyErr("daisy_capture_start returning FAILURE rc=\(rc)")
        g_capture = nil
    }
    return rc
}

@_cdecl("daisy_capture_stop")
public func daisy_capture_stop() -> Int32 {
    guard let cap = g_capture else {
        daisyWarn("daisy_capture_stop: no active capture")
        return -1
    }
    daisyLog("daisy_capture_stop")
    cap.stop()
    g_capture = nil
    return 0
}

// Output-device classification for AEC routing. Returns a code consumed by
// Rust `routing::classify_macos_transport`:
//   0 unknown · 1 builtin-speaker · 2 builtin-headphones · 3 bluetooth · 4 usb
//   5 hdmi/displayport · 6 virtual/aggregate
@_cdecl("daisy_default_output_class")
public func daisy_default_output_class() -> Int32 {
    var devAddr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var dev = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let st = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &devAddr, 0, nil, &size, &dev)
    if st != noErr || dev == kAudioObjectUnknown { return 0 }

    var transAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyTransportType,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var transport: UInt32 = 0
    var tsize = UInt32(MemoryLayout<UInt32>.size)
    if AudioObjectGetPropertyData(dev, &transAddr, 0, nil, &tsize, &transport) != noErr {
        return 0
    }

    switch transport {
    case kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE:
        return 3
    case kAudioDeviceTransportTypeUSB:
        return 4
    case kAudioDeviceTransportTypeHDMI, kAudioDeviceTransportTypeDisplayPort:
        return 5
    case kAudioDeviceTransportTypeBuiltIn:
        return daisyBuiltInOutputIsHeadphones(dev) ? 2 : 1
    case kAudioDeviceTransportTypeVirtual, kAudioDeviceTransportTypeAggregate:
        return 6
    default:
        return 0
    }
}

// Built-in output: distinguish internal speakers from the 3.5mm headphone jack
// via the active output data-source name.
private func daisyBuiltInOutputIsHeadphones(_ dev: AudioObjectID) -> Bool {
    var dsAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDataSource,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain)
    var dataSource: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    if AudioObjectGetPropertyData(dev, &dsAddr, 0, nil, &size, &dataSource) != noErr {
        return false
    }
    var name: Unmanaged<CFString>?
    var translation = AudioValueTranslation(
        mInputData: &dataSource,
        mInputDataSize: UInt32(MemoryLayout<UInt32>.size),
        mOutputData: &name,
        mOutputDataSize: UInt32(MemoryLayout<Unmanaged<CFString>?>.size))
    var nameAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDataSourceNameForIDCFString,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain)
    var nsize = UInt32(MemoryLayout<AudioValueTranslation>.size)
    if AudioObjectGetPropertyData(dev, &nameAddr, 0, nil, &nsize, &translation) != noErr {
        return false
    }
    guard let cf = name?.takeRetainedValue() else { return false }
    return (cf as String).lowercased().contains("headphone")
}

// Classify a specific INPUT (mic) device by transport type — same codes as
// daisy_default_output_class. A Bluetooth/headset mic can't pick up speaker
// output, so AEC is unnecessary even on speaker output. deviceID 0 = default
// input.
@_cdecl("daisy_input_class")
public func daisy_input_class(_ deviceID: UInt32) -> Int32 {
    var dev = AudioObjectID(deviceID)
    if deviceID == 0 {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        if AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &dev) != noErr
        {
            return 0
        }
    }
    if dev == kAudioObjectUnknown { return 0 }

    var transAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyTransportType,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var transport: UInt32 = 0
    var tsize = UInt32(MemoryLayout<UInt32>.size)
    if AudioObjectGetPropertyData(dev, &transAddr, 0, nil, &tsize, &transport) != noErr {
        return 0
    }
    switch transport {
    case kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE:
        return 3 // bluetooth
    case kAudioDeviceTransportTypeUSB:
        return 4 // usb (ambiguous)
    case kAudioDeviceTransportTypeBuiltIn:
        return 1 // built-in mic — not a headset
    default:
        return 0
    }
}

@_cdecl("daisy_permission_status")
public func daisy_permission_status() -> Int32 {
    // 0 not-determined / 1 granted / 2 denied. Audio-only path: microphone TCC.
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized: return 1
    case .denied, .restricted: return 2
    case .notDetermined: return 0
    @unknown default: return 0
    }
}

// ── Mic level meter (shares DaisyMicEngine; meter mode = RMS only) ──────────

var g_meter: DaisyMicEngine?

@_cdecl("daisy_mic_meter_start")
public func daisy_mic_meter_start(
    _ deviceID: UInt32,
    _ ctx: UnsafeMutableRawPointer?,
    _ onRms: @escaping RmsCallback
) -> Int32 {
    daisyLog("daisy_mic_meter_start(deviceID=\(deviceID))")
    let m = DaisyMicEngine(ctx: ctx, onPcm: nil, onRms: onRms)
    g_meter = m
    let rc = m.start(deviceID: deviceID)
    if rc != 0 {
        daisyErr("daisy_mic_meter_start returning FAILURE rc=\(rc) for deviceID=\(deviceID)")
        g_meter = nil
    }
    return rc
}

@_cdecl("daisy_mic_meter_stop")
public func daisy_mic_meter_stop() {
    daisyLog("daisy_mic_meter_stop")
    g_meter?.stop()
    g_meter = nil
}
