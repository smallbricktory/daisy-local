import Foundation
import CoreAudio
import AudioToolbox
import AVFoundation

// track: 0 = system audio, 1 = microphone
public typealias PcmCallback = @convention(c) (UnsafeMutableRawPointer?, Int32, UnsafePointer<Float>?, Int32, Int32, Int32, UInt64) -> Void
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
// "macos-shim:". Level: 0 info · 1 warn · 2 error. Every CoreAudio
// error path below calls daisyLog so failures leave a trail instead of vanishing
// into a bare return code.
public typealias LogCallback = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void
var g_log: LogCallback?

private var daisyTimebase = mach_timebase_info_data_t(numer: 0, denom: 0)

/// Mach ticks to nanoseconds. The timebase is queried once; a failed query
/// leaves numer/denom zero and yields 0, which the Rust side reads as
/// "no stamp".
private func daisyHostNs(_ ticks: UInt64) -> UInt64 {
    if daisyTimebase.denom == 0 {
        mach_timebase_info(&daisyTimebase)
    }
    if daisyTimebase.denom == 0 { return 0 }
    return UInt64((UInt64(ticks) &* UInt64(daisyTimebase.numer)) / UInt64(daisyTimebase.denom))
}

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

/// kAudioDevicePropertyDeviceUID. Stable across replug and across the
/// renumbering that any device arriving or leaving causes.
func daisyDeviceUID(_ id: AudioDeviceID) -> String? {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var cf: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    let st = AudioObjectGetPropertyData(id, &addr, 0, nil, &size, &cf)
    guard st == noErr, let uid = cf?.takeRetainedValue() else { return nil }
    return uid as String
}

/// The device currently carrying `uid`, or 0 when it is not attached.
func daisyDeviceForUID(_ uid: String) -> AudioDeviceID {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size) == noErr, size > 0
    else { return 0 }
    var ids = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
    guard AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &ids) == noErr
    else { return 0 }
    for dev in ids where daisyDeviceUID(dev) == uid { return dev }
    return 0
}

/// Blocks until the microphone authorization decision is known.
/// 0 = authorized, -4 = denied. Authorization is a property of the process,
/// so this is asked once per start rather than per backend.
func daisyEnsureMicAuthorized() -> Int32 {
    let auth = AVCaptureDevice.authorizationStatus(for: .audio)
    daisyLog("mic auth: status=\(auth.rawValue) (0=notDetermined 1=restricted 2=denied 3=authorized)")
    switch auth {
    case .authorized:
        return 0
    case .notDetermined:
        daisyLog("mic auth: requesting authorization (blocking until user answers)")
        let sem = DispatchSemaphore(value: 0)
        AVCaptureDevice.requestAccess(for: .audio) { _ in sem.signal() }
        sem.wait()
        let granted = AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
        daisyLog("mic auth: \(granted ? "GRANTED" : "DENIED") by user")
        if granted { return 0 }
    case .denied, .restricted:
        break
    @unknown default:
        break
    }
    daisyErr("mic start FAILED: mic permission denied (rc=-4)")
    return -4
}

/// Newest-sample host time for a buffer the HAL stamped at its FIRST sample.
/// The `on_pcm` bridge contract is the newest sample's time; 0 means the
/// buffer carried no usable stamp.
func daisyNewestSampleNs(_ ts: UnsafePointer<AudioTimeStamp>, frames: UInt32, sampleRate: Int32) -> UInt64 {
    var ns = daisyHostNs(ts.pointee.mHostTime)
    if ns != 0 && frames > 1 && sampleRate > 0 {
        ns &+= UInt64(frames - 1) &* 1_000_000_000 / UInt64(sampleRate)
    }
    return ns
}

/// System default device for the given selector; 0 when it cannot be resolved.
func daisyDefaultDevice(_ selector: AudioObjectPropertySelector) -> AudioDeviceID {
    var addr = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var dev = AudioDeviceID(0)
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    let st = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &dev)
    return st == noErr ? dev : 0
}

/// Device's nominal hardware sample rate; 0 when it cannot be read.
func daisyNominalSampleRate(_ dev: AudioDeviceID) -> Double {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyNominalSampleRate,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var rate = 0.0
    var size = UInt32(MemoryLayout<Double>.size)
    let st = AudioObjectGetPropertyData(dev, &addr, 0, nil, &size, &rate)
    return st == noErr ? rate : 0
}

/// CoreAudio transport type (`kAudioDeviceTransportType*`); 0 when unreadable.
func daisyTransportType(_ dev: AudioDeviceID) -> UInt32 {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyTransportType,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var transport: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    let st = AudioObjectGetPropertyData(dev, &addr, 0, nil, &size, &transport)
    return st == noErr ? transport : 0
}

/// Total channels on a scope; 0 means the device has none there.
func daisyChannelCount(_ dev: AudioDeviceID, scope: AudioObjectPropertyScope) -> Int {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(dev, &addr, 0, nil, &size) == noErr, size > 0 else {
        return 0
    }
    let buf = UnsafeMutableRawPointer.allocate(byteCount: Int(size), alignment: 16)
    defer { buf.deallocate() }
    guard AudioObjectGetPropertyData(dev, &addr, 0, nil, &size, buf) == noErr else { return 0 }
    let abl = UnsafeMutableAudioBufferListPointer(buf.assumingMemoryBound(to: AudioBufferList.self))
    return abl.reduce(0) { $0 + Int($1.mNumberChannels) }
}

/// First built-in transport output device, or 0 when there is none.
func daisyBuiltInOutputDevice() -> AudioDeviceID {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size) == noErr, size > 0
    else { return 0 }
    var ids = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
    guard AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &ids) == noErr
    else { return 0 }
    for dev in ids where daisyTransportType(dev) == kAudioDeviceTransportTypeBuiltIn {
        if daisyChannelCount(dev, scope: kAudioObjectPropertyScopeOutput) > 0 { return dev }
    }
    return 0
}

// ── Microphone capture ─────────────────────────────────────────────────────
// One class serves both recording (emit PCM via onPcm) and the live level bar
// (emit peak via onRms), on either backend. Mic-only: needs just the
// microphone permission. Device death surfaces as AudioUnitRender error codes
// on our own callback; the Rust health monitor and supervisor drive recovery.

/// One started microphone. Which backend served it is a field, not a type:
/// everything after the start call — rendering, delivery, teardown — is
/// identical for both backends.
final class DaisyMic {
    let ctx: UnsafeMutableRawPointer?
    let onPcm: PcmCallback?
    let onRms: RmsCallback?
    var unit: AudioUnit?
    var backend: Int32 = DAISY_MIC_BACKEND_AUHAL
    /// Device id this mic was asked for (0 = system default), as passed to
    /// `startAUHAL`/`startVPIO`. An escalation restarts the same request.
    var requestedDeviceID: UInt32 = 0
    var sampleRate: Int32 = 0
    var channels: Int32 = 1
    var buf: UnsafeMutablePointer<Float>?
    var bufCapacity: Int = 0
    var renderErrors: UInt32 = 0
    /// UID of the pinned VPIO output whose input scope this unit is
    /// responsible for; empty when it pinned an output that was already
    /// input-capable.
    var phantomOutput = ""

    init(ctx: UnsafeMutableRawPointer?, onPcm: PcmCallback?, onRms: RmsCallback?) {
        self.ctx = ctx; self.onPcm = onPcm; self.onRms = onRms
    }

    func startAUHAL(deviceID: UInt32) -> Int32 {
        let mode = onPcm != nil ? "capture" : "meter"
        backend = DAISY_MIC_BACKEND_AUHAL
        requestedDeviceID = deviceID
        daisyLog("mic auhal start: deviceID=\(deviceID) (0=system-default) mode=\(mode)")

        var dev = AudioDeviceID(deviceID)
        if dev == 0 {
            dev = daisyDefaultDevice(kAudioHardwarePropertyDefaultInputDevice)
            if dev == 0 {
                daisyErr("mic auhal start FAILED: no default input device (rc=-2)")
                return -2
            }
        }

        // An output-only device opened as a microphone starts cleanly and then
        // delivers nothing, so a meter or a recording on it looks alive and is
        // silent. Refuse it here rather than report a running stream.
        if daisyChannelCount(dev, scope: kAudioDevicePropertyScopeInput) == 0 {
            daisyErr("mic auhal start FAILED: device \(dev) (\(deviceName(dev))) has no input channels (rc=-2)")
            return -2
        }

        var desc = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0, componentFlagsMask: 0)
        guard let comp = AudioComponentFindNext(nil, &desc) else {
            daisyErr("mic auhal start FAILED: no HALOutput component (rc=-1)")
            return -1
        }
        var au: AudioUnit?
        var st = AudioComponentInstanceNew(comp, &au)
        guard st == noErr, let unit = au else {
            daisyErr("mic auhal start FAILED: AudioComponentInstanceNew rc=\(osStatusString(st))")
            return st
        }
        self.unit = unit

        var one: UInt32 = 1
        var zero: UInt32 = 0
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO,
                                  kAudioUnitScope_Input, 1, &one, 4)
        if st != noErr { teardown(); daisyErr("mic auhal: EnableIO(input) rc=\(osStatusString(st))"); return st }
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_EnableIO,
                                  kAudioUnitScope_Output, 0, &zero, 4)
        if st != noErr { teardown(); daisyErr("mic auhal: EnableIO(output off) rc=\(osStatusString(st))"); return st }
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice,
                                  kAudioUnitScope_Global, 0, &dev,
                                  UInt32(MemoryLayout<AudioDeviceID>.size))
        if st != noErr {
            teardown()
            daisyErr("mic auhal start FAILED: CurrentDevice=\(dev) rc=\(osStatusString(st)) — device likely stale/absent/aggregate")
            return st
        }
        daisyLog("mic auhal: HAL input device set to \(dev) (\(deviceName(dev)))")

        // Hardware-side format: the client format must match its sample rate
        // (AUHAL does not rate-convert on the input side).
        var hw = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        st = AudioUnitGetProperty(unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Input, 1, &hw, &size)
        if st != noErr || hw.mSampleRate <= 0 || hw.mChannelsPerFrame == 0 {
            teardown()
            daisyErr("mic auhal start FAILED: hw format rc=\(osStatusString(st)) sr=\(hw.mSampleRate) ch=\(hw.mChannelsPerFrame) (rc=-2)")
            return st != noErr ? st : -2
        }

        func clientFormat(_ ch: UInt32) -> AudioStreamBasicDescription {
            AudioStreamBasicDescription(
                mSampleRate: hw.mSampleRate,
                mFormatID: kAudioFormatLinearPCM,
                mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                mBytesPerPacket: 4 * ch,
                mFramesPerPacket: 1,
                mBytesPerFrame: 4 * ch,
                mChannelsPerFrame: ch,
                mBitsPerChannel: 32,
                mReserved: 0)
        }
        // Mono client side when the converter allows it; otherwise take the
        // hardware channel count interleaved (Rust downmixes).
        var chosen: UInt32 = 1
        var client = clientFormat(1)
        st = AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Output, 1, &client,
                                  UInt32(MemoryLayout<AudioStreamBasicDescription>.size))
        if st != noErr {
            chosen = hw.mChannelsPerFrame
            client = clientFormat(chosen)
            st = AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat,
                                      kAudioUnitScope_Output, 1, &client,
                                      UInt32(MemoryLayout<AudioStreamBasicDescription>.size))
            if st != noErr {
                teardown()
                daisyErr("mic auhal start FAILED: client format rc=\(osStatusString(st))")
                return st
            }
        }
        self.sampleRate = Int32(hw.mSampleRate)
        self.channels = Int32(chosen)

        var cb = AURenderCallbackStruct(
            inputProc: daisyMicRenderThunk,
            inputProcRefCon: Unmanaged.passUnretained(self).toOpaque())
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_SetInputCallback,
                                  kAudioUnitScope_Global, 0, &cb,
                                  UInt32(MemoryLayout<AURenderCallbackStruct>.size))
        if st != noErr { teardown(); daisyErr("mic auhal: SetInputCallback rc=\(osStatusString(st))"); return st }

        bufCapacity = 8192 * Int(chosen)
        buf = UnsafeMutablePointer<Float>.allocate(capacity: bufCapacity)

        st = AudioUnitInitialize(unit)
        if st != noErr { teardown(); daisyErr("mic auhal: AudioUnitInitialize rc=\(osStatusString(st))"); return st }
        st = AudioOutputUnitStart(unit)
        if st != noErr { teardown(); daisyErr("mic auhal: AudioOutputUnitStart rc=\(osStatusString(st)) — device likely contended"); return st }
        daisyLog("mic auhal STARTED: deviceID=\(dev) mode=\(mode) sr=\(sampleRate) ch=\(channels)")
        return 0
    }

    /// VoiceProcessingIO: joins the system voice-processing session; capture
    /// keeps delivering while other processes run their own VPIO clients —
    /// but silences every other process's RAW capture on the device, so a
    /// session only runs this after escalation proves another VPIO client
    /// already silenced our raw capture.
    /// Duplex by construction — the input element carries the mic,
    /// the output element is pinned to an output device as the echo reference
    /// and renders silence. Both elements must be pinned before
    /// AudioUnitInitialize; element (not scope) selects which.
    /// `deviceID` 0 = system default input; `outputDeviceID` 0 = system
    /// default output.
    func startVPIO(deviceID: UInt32, outputDeviceID: UInt32 = 0) -> Int32 {
        let mode = onPcm != nil ? "capture" : "meter"
        backend = DAISY_MIC_BACKEND_VPIO
        requestedDeviceID = deviceID
        daisyLog("mic vpio start: deviceID=\(deviceID) (0=system-default) outDev=\(outputDeviceID) mode=\(mode)")

        var inDev = AudioDeviceID(deviceID)
        if inDev == 0 {
            inDev = daisyDefaultDevice(kAudioHardwarePropertyDefaultInputDevice)
            if inDev == 0 {
                daisyErr("mic vpio start FAILED: no default input device (rc=-2)")
                return -2
            }
        }
        var outDev = AudioDeviceID(outputDeviceID)
        if outDev == 0 {
            outDev = daisyDefaultDevice(kAudioHardwarePropertyDefaultOutputDevice)
            if outDev == 0 {
                daisyErr("mic vpio start FAILED: no default output device (rc=-2)")
                return -2
            }
        }
        // Answered from the memo of what this device looked like before any
        // unit of ours pinned it. A held unit keeps its pinned output's input
        // scope open for the life of the process, so asking CoreAudio directly
        // reports every previously-pinned output as a capture device.
        let outWasInputCapable = outputWasInputCapableBeforePinning(outDev)

        // VPIO builds a private aggregate from the pinned pair; an aggregate
        // device cannot be a member of one.
        if daisyTransportType(inDev) == kAudioDeviceTransportTypeAggregate {
            daisyWarn("mic vpio: input \(inDev) (\(deviceName(inDev))) is an aggregate — not pinnable (rc=-3)")
            return -3
        }

        var desc = AudioComponentDescription(
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_VoiceProcessingIO,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0, componentFlagsMask: 0)
        guard let comp = AudioComponentFindNext(nil, &desc) else {
            daisyErr("mic vpio start FAILED: no VoiceProcessingIO component (rc=-1)")
            return -1
        }
        var au: AudioUnit?
        var st = AudioComponentInstanceNew(comp, &au)
        guard st == noErr, let unit = au else {
            daisyErr("mic vpio start FAILED: AudioComponentInstanceNew rc=\(osStatusString(st))")
            return st
        }
        self.unit = unit

        // Before the pin, not after the start: pinning is what grafts the
        // input scope, and a cold VoiceProcessingIO start takes seconds —
        // every enumeration in between would offer the output as a mic.
        // Every failure below tears down, which releases this.
        if !outWasInputCapable, let outUID = daisyDeviceUID(outDev) {
            phantomOutput = outUID
            retainPhantomInput(outUID)
        }

        let idSize = UInt32(MemoryLayout<AudioDeviceID>.size)
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice,
                                  kAudioUnitScope_Global, 1, &inDev, idSize)
        if st != noErr {
            teardown()
            daisyErr("mic vpio start FAILED: CurrentDevice(el1 in=\(inDev)) rc=\(osStatusString(st))")
            return st
        }
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice,
                                  kAudioUnitScope_Global, 0, &outDev, idSize)
        if st != noErr {
            teardown()
            daisyErr("mic vpio start FAILED: CurrentDevice(el0 out=\(outDev)) rc=\(osStatusString(st))")
            return st
        }
        daisyLog("mic vpio: pinned in=\(inDev) (\(deviceName(inDev))) out=\(outDev) (\(deviceName(outDev)))")

        // Mono Float32 at the input device's nominal rate: VPIO refuses stereo
        // client formats, and the rate must match the pinned input hardware.
        let rate = daisyNominalSampleRate(inDev)
        if rate <= 0 {
            teardown()
            daisyErr("mic vpio start FAILED: input device nominal rate \(rate) (rc=-2)")
            return -2
        }
        var fmt = AudioStreamBasicDescription(
            mSampleRate: rate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4,
            mChannelsPerFrame: 1, mBitsPerChannel: 32, mReserved: 0)
        let fmtSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        st = AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Output, 1, &fmt, fmtSize)
        if st != noErr {
            teardown()
            daisyErr("mic vpio start FAILED: client format el1 rc=\(osStatusString(st))")
            return st
        }
        st = AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat,
                                  kAudioUnitScope_Input, 0, &fmt, fmtSize)
        if st != noErr {
            teardown()
            daisyErr("mic vpio start FAILED: client format el0 rc=\(osStatusString(st))")
            return st
        }
        self.sampleRate = Int32(rate)
        self.channels = 1

        var cb = AURenderCallbackStruct(
            inputProc: daisyMicRenderThunk,
            inputProcRefCon: Unmanaged.passUnretained(self).toOpaque())
        st = AudioUnitSetProperty(unit, kAudioOutputUnitProperty_SetInputCallback,
                                  kAudioUnitScope_Global, 0, &cb,
                                  UInt32(MemoryLayout<AURenderCallbackStruct>.size))
        if st != noErr {
            teardown(); daisyErr("mic vpio: SetInputCallback rc=\(osStatusString(st))"); return st
        }
        // Nothing is ever played through this unit; the output element exists
        // only as the echo reference and must render silence.
        var render = AURenderCallbackStruct(inputProc: daisyVpioSilenceThunk, inputProcRefCon: nil)
        st = AudioUnitSetProperty(unit, kAudioUnitProperty_SetRenderCallback,
                                  kAudioUnitScope_Input, 0, &render,
                                  UInt32(MemoryLayout<AURenderCallbackStruct>.size))
        if st != noErr {
            teardown(); daisyErr("mic vpio: SetRenderCallback rc=\(osStatusString(st))"); return st
        }

        bufCapacity = 8192
        buf = UnsafeMutablePointer<Float>.allocate(capacity: bufCapacity)

        st = AudioUnitInitialize(unit)
        if st != noErr {
            teardown()
            // -10875 means this input/output PAIR cannot form the private
            // aggregate; the caller retries with another output, then AUHAL.
            daisyErr("mic vpio: AudioUnitInitialize rc=\(osStatusString(st)) in=\(inDev) out=\(outDev)")
            return st
        }
        // A VoiceProcessingIO unit attenuates every other app's output on the
        // paired device for as long as it runs. Our render element is silent,
        // so the unit classifies everything the user wants to hear — including
        // the meeting's far end — as duckable. `.min` is the quietest ducking
        // Apple exposes; advanced ducking keys on downlink voice activity,
        // which we never have.
        if #available(macOS 14.0, *) {
            var duck = AUVoiceIOOtherAudioDuckingConfiguration(
                mEnableAdvancedDucking: false,
                mDuckingLevel: .min)
            let dst = AudioUnitSetProperty(
                unit, kAUVoiceIOProperty_OtherAudioDuckingConfiguration,
                kAudioUnitScope_Global, 0, &duck,
                UInt32(MemoryLayout<AUVoiceIOOtherAudioDuckingConfiguration>.size))
            if dst != noErr {
                daisyWarn("mic vpio: ducking config rc=\(osStatusString(dst)) — other audio stays ducked")
            }
        }

        st = AudioOutputUnitStart(unit)
        if st != noErr {
            teardown()
            daisyErr("mic vpio: AudioOutputUnitStart rc=\(osStatusString(st)) — device likely contended")
            return st
        }
        // `.min` above still leaves ~8 dB of ducking; ramping the device back
        // to full cancels the rest. Every VPIO start re-engages ducking, so
        // this runs after every start; the restore outlives the unit's
        // undisposed hold.
        daisyUnduckOutput(outDev)
        if !phantomOutput.isEmpty {
            daisyLog("mic vpio: output \(outDev) (\(deviceName(outDev))) hidden from mic enumeration "
                     + "(input scope now \(daisyChannelCount(outDev, scope: kAudioDevicePropertyScopeInput))ch)")
        }
        daisyLog("mic vpio STARTED: in=\(inDev) (\(deviceName(inDev))) out=\(outDev) mode=\(mode) sr=\(sampleRate) ch=1")
        return 0
    }

    /// Log prefix: which backend actually served this mic.
    var label: String { backend == DAISY_MIC_BACKEND_VPIO ? "mic vpio" : "mic auhal" }

    func render(_ flags: UnsafeMutablePointer<AudioUnitRenderActionFlags>,
                _ ts: UnsafePointer<AudioTimeStamp>,
                _ frames: UInt32) -> OSStatus {
        guard let unit = unit, let data = buf else { return noErr }
        let needed = Int(frames) * Int(channels)
        if needed > bufCapacity {
            if renderErrors == 0 { daisyWarn("\(label): buffer too small (\(needed) > \(bufCapacity)) — dropping") }
            renderErrors &+= 1
            return noErr
        }
        var abl = AudioBufferList(
            mNumberBuffers: 1,
            mBuffers: AudioBuffer(
                mNumberChannels: UInt32(channels),
                mDataByteSize: UInt32(needed * 4),
                mData: UnsafeMutableRawPointer(data)))
        let st = AudioUnitRender(unit, flags, ts, 1, frames, &abl)
        if st != noErr {
            // Device death shows up here; log a short trail and let the Rust
            // health monitor drive recovery.
            if renderErrors < 3 || renderErrors % 500 == 0 {
                daisyWarn("\(label): AudioUnitRender rc=\(osStatusString(st)) (#\(renderErrors))")
            }
            renderErrors &+= 1
            return st
        }
        renderErrors = 0
        let ns = daisyNewestSampleNs(ts, frames: frames, sampleRate: sampleRate)
        if let onPcm = onPcm {
            onPcm(ctx, DAISY_TRACK_MIC, data, Int32(frames), channels, sampleRate, ns)
        }
        if let onRms = onRms {
            var peak: Float = 0
            for i in 0..<needed { peak = max(peak, abs(data[i])) }
            onRms(ctx, peak)
        }
        return noErr
    }

    private func teardown() {
        if let unit = unit {
            AudioOutputUnitStop(unit)
            AudioUnitUninitialize(unit)
            AudioComponentInstanceDispose(unit)
        }
        unit = nil
        buf?.deallocate()
        buf = nil
        bufCapacity = 0
        if !phantomOutput.isEmpty {
            releasePhantomInput(phantomOutput)
            phantomOutput = ""
        }
    }

    /// Stop and uninitialize, handing the bare AudioUnit instance to the
    /// caller undisposed. This object keeps nothing afterwards — in
    /// particular not `ctx`, which points at memory the Rust side frees.
    func detachUnit() -> AudioComponentInstance? {
        guard let unit = unit else { return nil }
        AudioOutputUnitStop(unit)
        AudioUnitUninitialize(unit)
        self.unit = nil
        buf?.deallocate()
        buf = nil
        bufCapacity = 0
        return unit
    }

    /// A VPIO unit is held undisposed rather than torn down (see
    /// `holdOneVpioUndisposed`); AUHAL is disposed outright.
    func stop() {
        daisyLog("\(label) stop")
        if backend == DAISY_MIC_BACKEND_VPIO {
            holdOneVpioUndisposed(self)
        } else {
            teardown()
        }
    }
}

private func daisyMicRenderThunk(
    _ refCon: UnsafeMutableRawPointer,
    _ flags: UnsafeMutablePointer<AudioUnitRenderActionFlags>,
    _ ts: UnsafePointer<AudioTimeStamp>,
    _ bus: UInt32,
    _ frames: UInt32,
    _ ioData: UnsafeMutablePointer<AudioBufferList>?
) -> OSStatus {
    let me = Unmanaged<DaisyMic>.fromOpaque(refCon).takeUnretainedValue()
    return me.render(flags, ts, frames)
}

private func daisyVpioSilenceThunk(
    _ refCon: UnsafeMutableRawPointer,
    _ flags: UnsafeMutablePointer<AudioUnitRenderActionFlags>,
    _ ts: UnsafePointer<AudioTimeStamp>,
    _ bus: UInt32,
    _ frames: UInt32,
    _ ioData: UnsafeMutablePointer<AudioBufferList>?
) -> OSStatus {
    guard let io = ioData else { return noErr }
    for b in UnsafeMutableAudioBufferListPointer(io) {
        if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) }
    }
    return noErr
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

        // UNIQUE per-instance UID: creating a device with a UID the previous
        // session's aggregate still holds (teardown in coreaudiod is
        // asynchronous) forces a reconciliation that can delay the IOProc by
        // minutes. A fresh UUID each start avoids the collision.
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
            (_, inInputData, inInputTime, _, _) in
            let abl = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInputData))
            guard let buf = abl.first, let mData = buf.mData else { return }
            let floatsInBuf0 = Int32(buf.mDataByteSize) / 4
            let floats = mData.assumingMemoryBound(to: Float.self)
            // Bridge contract: host_ns stamps the NEWEST sample. The IOProc's
            // input time is the FIRST sample's — adjust by the buffer span so
            // both tracks (mic AUHAL does the same) stamp the same end.
            let frames = abl.count > 1 ? floatsInBuf0 : floatsInBuf0 / ch
            var ns = daisyHostNs(inInputTime.pointee.mHostTime)
            if ns != 0 && frames > 1 && sr > 0 {
                ns &+= UInt64(frames - 1) &* 1_000_000_000 / UInt64(sr)
            }
            if abl.count > 1 {
                // Planar: each AudioBuffer is one channel. Forward channel 0.
                cb(ctxLocal, DAISY_TRACK_SYSTEM, floats, floatsInBuf0, 1, sr, ns)
            } else {
                // Interleaved (or mono): buffer 0 holds frames*channels floats.
                cb(ctxLocal, DAISY_TRACK_SYSTEM, floats, floatsInBuf0 / ch, ch, sr, ns)
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

// ── Capture session (decoupled mic + tap) ──────────────────────────────────
// The mic unit and the system tap have independent lifecycles: a mic switch
// touches only the mic AUHAL, a system reopen only the tap. Initial start is
// all-or-nothing and synchronous — onState fires before it returns.
// The session callbacks live OUTSIDE the units: a restart rebuilds a dead
// unit from the session, so they must survive that unit's teardown.

struct DaisySession {
    let ctx: UnsafeMutableRawPointer?
    let onPcm: PcmCallback
    let onState: StateCallback
}

var g_tap: DaisySystemTap?
var g_mic: DaisyMic?
var g_session: DaisySession?

// Mic backend policy for the active capture session. Raw AUHAL by default:
// a VPIO client silences every other process's raw capture on the shared
// input device, so voice processing is joined only when escalation proves
// another VPIO client already silenced us. Reset at every capture start/stop.
var g_micPreferVpio = false

// Which backend a started mic is running on. VPIO output is already
// echo-cancelled by the OS; AUHAL output is raw and needs software AEC.
let DAISY_MIC_BACKEND_AUHAL: Int32 = 0
let DAISY_MIC_BACKEND_VPIO: Int32 = 1

// On macOS 14 the first VoiceProcessingIO unit created in a process costs
// seconds, later ones are fast, and disposing the last one resets that state.
// One stopped unit is held undisposed so the next start within the session
// takes the fast path. Disposed at capture stop (`daisyDisposeHeldVpio`): an
// idle process must hold no voice-processing unit. Nothing reuses this
// instance — the slot exists only to keep the undisposed count off zero.
private let vpioHoldLock = NSLock()
private var g_vpioHeldUnit: AudioComponentInstance?
/// UID of the phantom-input hold that rides with `g_vpioHeldUnit`, released
/// when that unit is finally disposed.
private var g_vpioHeldPhantom = ""

// A running VoiceProcessingIO unit grafts an input scope onto the output
// device it pinned, and CoreAudio then reports that device as capture-capable:
// same owner as a real microphone's stream, terminal type 0, not hidden.
// Nothing in the device's properties tells the two apart, so the output
// devices our own VPIO units hold are recorded here and enumeration skips
// them. Refcounted: a meter and a capture can hold the same output at once.
let phantomInputLock = NSLock()
// Keyed by UID: unplugging any device renumbers the AudioObjectIDs of the
// ones that remain, and a hold keyed by the old number stops matching the
// device it was taken on.
var g_phantomInputDevices: [String: Int] = [:]

// Input capability as first observed per device UID, before any unit of ours
// pinned that device. A device's own capture hardware does not come and go, so
// the first answer is the lasting one.
var g_outputWasInputCapable: [String: Bool] = [:]

/// The memoized answer for `uid`, seeding it with `live` the first time the
/// device is seen. Later calls ignore `live`.
func memoInputCapable(uid: String, live: Bool) -> Bool {
    phantomInputLock.lock()
    defer { phantomInputLock.unlock() }
    if let known = g_outputWasInputCapable[uid] { return known }
    g_outputWasInputCapable[uid] = live
    return live
}

/// `AudioDeviceDuck` has no public header; resolved via `dlsym` so its
/// removal from a future macOS degrades to a log line, not a launch failure.
private typealias DaisyDuckFn = @convention(c) (
    AudioDeviceID, Float32, UnsafePointer<AudioTimeStamp>?, Float32
) -> OSStatus
private let daisyDuckFn: DaisyDuckFn? = {
    // RTLD_DEFAULT is (void*)-2 on macOS.
    guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "AudioDeviceDuck") else {
        return nil
    }
    return unsafeBitCast(sym, to: DaisyDuckFn.self)
}()

/// Ramps `device` back to full output level, cancelling VoiceProcessingIO's
/// ducking of every other app's audio on it.
func daisyUnduckOutput(_ device: AudioDeviceID) {
    guard let f = daisyDuckFn else {
        daisyWarn("mic vpio: AudioDeviceDuck unavailable — other audio stays ducked while VPIO runs")
        return
    }
    let st = f(device, 1.0, nil, 0.5)
    if st == noErr {
        daisyLog("mic vpio: output \(device) unducked (0.5s ramp)")
    } else {
        daisyWarn("mic vpio: AudioDeviceDuck(\(device)) rc=\(osStatusString(st)) — other audio may stay ducked")
    }
}

/// Whether `outDev` had an input scope of its own, from the first observation
/// made of it. Devices with no UID are answered live and not memoized.
func outputWasInputCapableBeforePinning(_ outDev: AudioDeviceID) -> Bool {
    let live = daisyChannelCount(outDev, scope: kAudioDevicePropertyScopeInput) > 0
    guard let uid = daisyDeviceUID(outDev) else { return live }
    return memoInputCapable(uid: uid, live: live)
}

func retainPhantomInput(_ uid: String) {
    phantomInputLock.lock()
    defer { phantomInputLock.unlock() }
    g_phantomInputDevices[uid, default: 0] += 1
}

func releasePhantomInput(_ uid: String) {
    phantomInputLock.lock()
    defer { phantomInputLock.unlock() }
    guard let holds = g_phantomInputDevices[uid] else { return }
    if holds <= 1 {
        g_phantomInputDevices.removeValue(forKey: uid)
    } else {
        g_phantomInputDevices[uid] = holds - 1
    }
}

/// Hold count for `uid`; 0 when nothing holds it.
func phantomHoldCount(_ uid: String) -> Int {
    phantomInputLock.lock()
    defer { phantomInputLock.unlock() }
    return g_phantomInputDevices[uid] ?? 0
}

/// Devices whose input scope exists only because one of our VoiceProcessingIO
/// units pinned them as its output. Writes up to `cap` ids and returns how
/// many are held; a return above `cap` means the buffer was too small. The
/// ids are resolved per call, so they track renumbering; a held device that
/// has since gone is skipped.
@_cdecl("daisy_phantom_input_devices")
public func daisy_phantom_input_devices(_ out: UnsafeMutablePointer<UInt32>?, _ cap: Int32) -> Int32 {
    phantomInputLock.lock()
    let held = Array(g_phantomInputDevices.keys)
    phantomInputLock.unlock()
    var written = 0
    for uid in held {
        let dev = daisyDeviceForUID(uid)
        if dev == 0 { continue }
        if let out = out, written < Int(cap) { out[written] = UInt32(dev) }
        written += 1
    }
    return Int32(written)
}

/// Take the mic's bare AudioUnit into the hold slot instead of disposing it.
/// The previous occupant is disposed only after the new one is held, so the
/// count never drops to zero. The Swift wrapper — and with it the Rust
/// context pointer and callbacks — is released.
private func holdOneVpioUndisposed(_ mic: DaisyMic) {
    vpioHoldLock.lock()
    defer { vpioHoldLock.unlock() }
    guard let unit = mic.detachUnit() else {
        daisyWarn("mic vpio stop: no unit to hold")
        return
    }
    let previous = g_vpioHeldUnit
    let previousPhantom = g_vpioHeldPhantom
    g_vpioHeldUnit = unit
    // An uninitialized unit still holds its pinned output's input scope open,
    // so the hold on that device travels with the unit rather than ending
    // here. `detachUnit` left it registered for exactly this handover.
    g_vpioHeldPhantom = mic.phantomOutput
    mic.phantomOutput = ""
    if let previous = previous { AudioComponentInstanceDispose(previous) }
    if !previousPhantom.isEmpty { releasePhantomInput(previousPhantom) }
    daisyLog("mic vpio held undisposed")
}

/// Raw AUHAL unless `preferVpio`, which tries VPIO with the requested mic and
/// the default output, then VPIO with the built-in output, then AUHAL.
/// `-10875` at Initialize means the device PAIR cannot aggregate, not that
/// the mic is unusable. A permission denial (-4) short-circuits: no backend
/// can fix it.
func daisyStartMicWithLadder(
    ctx: UnsafeMutableRawPointer?,
    onPcm: PcmCallback?,
    onRms: RmsCallback?,
    deviceID: UInt32,
    preferVpio: Bool
) -> (mic: DaisyMic?, rc: Int32) {
    // Authorization is per process, so it is settled once — before any
    // backend is built and regardless of which rung ends up serving.
    let authRc = daisyEnsureMicAuthorized()
    if authRc != 0 { return (nil, authRc) }

    if preferVpio {
        let defaultOut = daisyDefaultDevice(kAudioHardwarePropertyDefaultOutputDevice)
        let vpio = DaisyMic(ctx: ctx, onPcm: onPcm, onRms: onRms)
        var rc = vpio.startVPIO(deviceID: deviceID, outputDeviceID: UInt32(defaultOut))
        if rc == 0 { return (vpio, 0) }

        // Only kAudioUnitErr_FailedInitialization means "this input/output PAIR
        // cannot aggregate" — the one failure a different output can fix. Every
        // other code (no device, aggregate input, contended device) reproduces
        // identically on a second attempt, and each attempt costs a full
        // AudioUnit lifecycle.
        let builtIn = daisyBuiltInOutputDevice()
        if rc == kAudioUnitErr_FailedInitialization && builtIn != 0 && builtIn != defaultOut {
            let retry = DaisyMic(ctx: ctx, onPcm: onPcm, onRms: onRms)
            let retryRc = retry.startVPIO(deviceID: deviceID, outputDeviceID: UInt32(builtIn))
            if retryRc == 0 {
                daisyLog("mic ladder: VPIO paired with built-in output \(builtIn) after rc=\(rc)")
                return (retry, 0)
            }
            rc = retryRc
        }
        daisyWarn("mic ladder: VPIO unavailable for device \(deviceID) (rc=\(rc)) — falling back to AUHAL (no OS echo cancellation)")
    }

    let auhal = DaisyMic(ctx: ctx, onPcm: onPcm, onRms: onRms)
    let arc = auhal.startAUHAL(deviceID: deviceID)
    if arc == 0 { return (auhal, 0) }
    return (nil, arc)
}

@_cdecl("daisy_capture_start")
public func daisy_capture_start(
    _ wantMic: Int32,
    _ micDeviceID: UInt32,
    _ ctx: UnsafeMutableRawPointer?,
    _ onPcm: @escaping PcmCallback,
    _ onState: @escaping StateCallback,
    _ outBackend: UnsafeMutablePointer<Int32>?
) -> Int32 {
    daisyLog("daisy_capture_start(wantMic=\(wantMic), micDeviceID=\(micDeviceID))")
    g_micPreferVpio = false
    g_session = DaisySession(ctx: ctx, onPcm: onPcm, onState: onState)
    let tap = DaisySystemTap(ctx: ctx, onPcm: onPcm)
    let st = tap.start()
    if st != noErr {
        daisyErr("capture start: system tap failed (rc=\(osStatusString(st))) — whole session fails")
        g_session = nil
        onState(ctx, DAISY_STATE_ERROR)
        return st
    }
    g_tap = tap
    if wantMic != 0 {
        let (mic, mst) = daisyStartMicWithLadder(
            ctx: ctx, onPcm: onPcm, onRms: nil, deviceID: micDeviceID,
            preferVpio: g_micPreferVpio)
        guard let mic = mic, mst == 0 else {
            // -2 = no input channels, -4 = mic authorization denied: both are
            // "we can't get the mic" → surface as a permission problem.
            let denied = (mst == -2 || mst == -4)
            daisyErr("capture start: mic failed (rc=\(mst)) → \(denied ? "PERM_DENIED" : "ERROR"); tearing down system tap")
            tap.stop()
            g_tap = nil
            g_session = nil
            onState(ctx, denied ? DAISY_STATE_PERM_DENIED : DAISY_STATE_ERROR)
            return mst
        }
        g_mic = mic
        outBackend?.pointee = mic.backend
    }
    daisyLog("capture STARTED: state=RUNNING")
    onState(ctx, DAISY_STATE_RUNNING)
    return 0
}

@_cdecl("daisy_capture_stop")
public func daisy_capture_stop() -> Int32 {
    if g_tap == nil && g_mic == nil {
        daisyWarn("daisy_capture_stop: no active capture")
        return -1
    }
    daisyLog("daisy_capture_stop")
    g_mic?.stop()
    g_mic = nil
    g_tap?.stop()
    g_tap = nil
    daisyDisposeHeldVpio()
    g_micPreferVpio = false
    if let s = g_session { s.onState(s.ctx, DAISY_STATE_STOPPED) }
    g_session = nil
    return 0
}

/// Dispose the held VPIO unit and release its phantom output registration.
/// Called when capture ends: an idle process must hold no voice-processing
/// unit, so other apps' raw capture is never affected between sessions.
func daisyDisposeHeldVpio() {
    vpioHoldLock.lock()
    defer { vpioHoldLock.unlock() }
    if let unit = g_vpioHeldUnit {
        AudioComponentInstanceDispose(unit)
        g_vpioHeldUnit = nil
        daisyLog("mic vpio: held unit disposed at capture stop")
    }
    if !g_vpioHeldPhantom.isEmpty {
        releasePhantomInput(g_vpioHeldPhantom)
        g_vpioHeldPhantom = ""
    }
}

/// Mic-only restart on a (possibly different) device; the system tap keeps
/// running untouched. Synchronous; 0 = the new mic is live.
@_cdecl("daisy_mic_restart")
public func daisy_mic_restart(
    _ micDeviceID: UInt32,
    _ outBackend: UnsafeMutablePointer<Int32>?
) -> Int32 {
    guard let s = g_session else {
        daisyWarn("daisy_mic_restart: no active capture session")
        return -1
    }
    daisyLog("daisy_mic_restart(micDeviceID=\(micDeviceID))")
    g_mic?.stop()
    g_mic = nil
    let (mic, rc) = daisyStartMicWithLadder(
        ctx: s.ctx, onPcm: s.onPcm, onRms: nil, deviceID: micDeviceID,
        preferVpio: g_micPreferVpio)
    if let mic = mic, rc == 0 {
        g_mic = mic
        outBackend?.pointee = mic.backend
    } else {
        daisyErr("daisy_mic_restart: start rc=\(rc) — mic silent, session continues")
    }
    return rc
}

/// Restart the active session mic with voice processing preferred, after the
/// raw capture was silenced by another process's VPIO client. One-way for the
/// session: `g_micPreferVpio` stays set so device switches keep VPIO.
@_cdecl("daisy_mic_escalate")
public func daisy_mic_escalate(
    _ outBackend: UnsafeMutablePointer<Int32>?
) -> Int32 {
    guard let s = g_session, let current = g_mic else {
        daisyWarn("daisy_mic_escalate: no active session mic")
        return -1
    }
    let deviceID = current.requestedDeviceID
    daisyLog("daisy_mic_escalate(deviceID=\(deviceID))")
    g_micPreferVpio = true
    current.stop()
    g_mic = nil
    let (mic, rc) = daisyStartMicWithLadder(
        ctx: s.ctx, onPcm: s.onPcm, onRms: nil, deviceID: deviceID,
        preferVpio: true)
    if let mic = mic, rc == 0 {
        g_mic = mic
        outBackend?.pointee = mic.backend
    } else {
        daisyErr("daisy_mic_escalate: start rc=\(rc) — mic silent, session continues")
    }
    return rc
}

/// Pids of OTHER processes currently running audio input. Sees every
/// CoreAudio client process (apps and daemons both). Returns the count; at
/// most `cap` pids are written to `buf`.
@_cdecl("daisy_capturing_pids")
public func daisy_capturing_pids(
    _ buf: UnsafeMutablePointer<Int32>?,
    _ cap: Int32
) -> Int32 {
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyProcessObjectList,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size) == noErr,
        size > 0
    else { return 0 }
    var objects = [AudioObjectID](
        repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    guard AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &objects) == noErr
    else { return 0 }

    let selfPid = Int32(getpid())
    var count: Int32 = 0
    for obj in objects {
        var runningInput: UInt32 = 0
        var sz = UInt32(MemoryLayout<UInt32>.size)
        var a = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyIsRunningInput,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        guard AudioObjectGetPropertyData(obj, &a, 0, nil, &sz, &runningInput) == noErr,
            runningInput != 0
        else { continue }
        var pid: pid_t = 0
        var psz = UInt32(MemoryLayout<pid_t>.size)
        a.mSelector = kAudioProcessPropertyPID
        guard AudioObjectGetPropertyData(obj, &a, 0, nil, &psz, &pid) == noErr,
            Int32(pid) != selfPid
        else { continue }
        if count < cap, let buf = buf { buf[Int(count)] = Int32(pid) }
        count += 1
    }
    return count
}

/// System-tap-only restart; the mic keeps running untouched.
@_cdecl("daisy_tap_restart")
public func daisy_tap_restart() -> Int32 {
    guard let s = g_session else {
        daisyWarn("daisy_tap_restart: no active capture session")
        return -1
    }
    daisyLog("daisy_tap_restart")
    g_tap?.stop()
    g_tap = nil
    let tap = DaisySystemTap(ctx: s.ctx, onPcm: s.onPcm)
    let rc = tap.start()
    if rc == 0 {
        g_tap = tap
    } else {
        daisyErr("daisy_tap_restart: start rc=\(osStatusString(rc)) — system silent, session continues")
    }
    return rc
}

// Output-device classification for AEC routing. Returns a code consumed by
// Rust `routing::classify_macos_transport`:
//   0 unknown · 1 builtin-speaker · 2 builtin-headphones · 3 bluetooth · 4 usb
//   5 hdmi/displayport · 6 virtual/aggregate
@_cdecl("daisy_default_output_class")
public func daisy_default_output_class() -> Int32 {
    let dev = daisyDefaultDevice(kAudioHardwarePropertyDefaultOutputDevice)
    if dev == 0 { return 0 }
    let transport = daisyTransportType(dev)
    if transport == 0 { return 0 }

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
    var nameAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDataSourceNameForIDCFString,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain)
    // CoreAudio reads through the pointers in AudioValueTranslation during
    // the property call, so they must stay valid past the struct's own
    // initializer — `&x` inline does not.
    var name: Unmanaged<CFString>?
    let ok = withUnsafeMutablePointer(to: &dataSource) { inPtr in
        withUnsafeMutablePointer(to: &name) { outPtr in
            var translation = AudioValueTranslation(
                mInputData: UnsafeMutableRawPointer(inPtr),
                mInputDataSize: UInt32(MemoryLayout<UInt32>.size),
                mOutputData: UnsafeMutableRawPointer(outPtr),
                mOutputDataSize: UInt32(MemoryLayout<Unmanaged<CFString>?>.size))
            var nsize = UInt32(MemoryLayout<AudioValueTranslation>.size)
            return AudioObjectGetPropertyData(dev, &nameAddr, 0, nil, &nsize, &translation) == noErr
        }
    }
    guard ok, let cf = name?.takeRetainedValue() else { return false }
    return (cf as String).lowercased().contains("headphone")
}

// Classify a specific INPUT (mic) device by transport type — same codes as
// daisy_default_output_class. A Bluetooth/headset mic can't pick up speaker
// output, so AEC is unnecessary even on speaker output. deviceID 0 = default
// input.
@_cdecl("daisy_input_class")
public func daisy_input_class(_ deviceID: UInt32) -> Int32 {
    let dev = deviceID == 0
        ? daisyDefaultDevice(kAudioHardwarePropertyDefaultInputDevice)
        : AudioDeviceID(deviceID)
    if dev == 0 || dev == kAudioObjectUnknown { return 0 }

    let transport = daisyTransportType(dev)
    if transport == 0 {
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

// ── Mic preview (shares the capture start ladder; no system tap) ──────────
// Serves the picker's level meter (RMS only) and the one-shot calibration
// capture (PCM). Both run on the same backend recording would pick — a
// preview on a different backend would show different levels, and a
// calibration recorded in the other capture domain would not match.
// The Rust side spawns an independent OS thread per start/stop call (mic
// picker device switches overlap: the old device's stop and the new
// device's start race on separate threads with no ordering guarantee). A
// single shared instance keyed only by "the current one" is not safe here:
// a start racing ahead of the matching stop would silently orphan the prior
// AudioUnit while its IO thread still holds an unretained reference to the
// now-deallocated Swift wrapper (the render callback uses
// `Unmanaged.passUnretained`, so ARC drops it the instant nothing else
// retains it) — the IO thread then renders into freed memory. Keying by a
// per-start token makes every start/stop pair independent: a stop can only
// ever tear down the exact instance it started, never a newer one that
// superseded it, and the lock makes each start/stop a single atomic step
// so no half-torn-down instance is ever visible to another thread.
private let previewLock = NSLock()
private var previewNextToken: Int64 = 1
private var previews: [Int64: DaisyMic] = [:]

@_cdecl("daisy_mic_preview_start")
public func daisy_mic_preview_start(
    _ deviceID: UInt32,
    _ ctx: UnsafeMutableRawPointer?,
    _ onPcm: PcmCallback?,
    _ onRms: RmsCallback?,
    _ outToken: UnsafeMutablePointer<Int64>?,
    _ outBackend: UnsafeMutablePointer<Int32>?
) -> Int32 {
    daisyLog("daisy_mic_preview_start(deviceID=\(deviceID), pcm=\(onPcm != nil))")
    // Built OUTSIDE the lock: a unit start can take seconds, and holding the
    // lock across that would serialize a picker device switch behind it — the
    // very overlap the per-token scheme exists to keep independent.
    // Always raw: recording starts raw, so preview levels match, and a idle
    // preview must never hold a VPIO unit that silences other apps' capture.
    let (m, rc) = daisyStartMicWithLadder(
        ctx: ctx, onPcm: onPcm, onRms: onRms, deviceID: deviceID,
        preferVpio: false)
    guard let m = m, rc == 0 else {
        daisyErr("daisy_mic_preview_start returning FAILURE rc=\(rc) for deviceID=\(deviceID)")
        return rc
    }
    previewLock.lock()
    defer { previewLock.unlock() }
    let token = previewNextToken
    previewNextToken &+= 1
    previews[token] = m
    outToken?.pointee = token
    outBackend?.pointee = m.backend
    return rc
}

@_cdecl("daisy_mic_preview_stop")
public func daisy_mic_preview_stop(_ token: Int64) {
    previewLock.lock()
    defer { previewLock.unlock() }
    daisyLog("daisy_mic_preview_stop(token=\(token))")
    previews.removeValue(forKey: token)?.stop()
}
