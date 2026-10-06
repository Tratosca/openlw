// Mac audio devices: default input/output, “OpenLW” device.
// CoreAudio APIs available since 10.0 (minimum 10.13).

import CoreAudio
import Foundation

enum MacAudio {
    /// UIDs published by HAL plugin (macos/plugin/src/OpenLWPlugIn.c): duplex device, or
    /// “OpenLW In” and “OpenLW Out” in two-device layout.
    static let livewireUID = "fr.francois-brille.openlw.device"
    static let livewireInUID = "fr.francois-brille.openlw.device.in"
    static let livewireOutUID = "fr.francois-brille.openlw.device.out"

    private static func address(_ selector: AudioObjectPropertySelector,
                                _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: 0) // Main element
    }

    private static func string(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector) -> String? {
        var addr = address(selector)
        var ref: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(id, &addr, 0, nil, &size, &ref) == noErr, let s = ref else { return nil }
        return s.takeRetainedValue() as String
    }

    static func devices() -> [AudioObjectID] {
        var addr = address(kAudioHardwarePropertyDevices)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size) == noErr else { return [] }
        var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &ids) == noErr else { return [] }
        return ids
    }

    static func name(_ id: AudioObjectID) -> String {
        string(id, kAudioObjectPropertyName) ?? "périphérique \(id)"
    }

    /// OpenLW input/output device if loaded by CoreAudio.
    static func livewireDevice(input: Bool) -> AudioObjectID? {
        let uids = [livewireUID, input ? livewireInUID : livewireOutUID]
        return devices().first { uids.contains(string($0, kAudioDevicePropertyDeviceUID) ?? "") }
    }

    private static func defaultSelector(input: Bool) -> AudioObjectPropertySelector {
        input ? kAudioHardwarePropertyDefaultInputDevice : kAudioHardwarePropertyDefaultOutputDevice
    }

    static func defaultDevice(input: Bool) -> AudioObjectID? {
        var addr = address(defaultSelector(input: input))
        var id = AudioObjectID(0)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &id) == noErr,
              id != 0 else { return nil }
        return id
    }

    /// Is OpenLW the Mac's default input/output?
    static func livewireIsDefault(input: Bool) -> Bool {
        guard let lw = livewireDevice(input: input) else { return false }
        return defaultDevice(input: input) == lw
    }

    @discardableResult
    static func setDefault(_ id: AudioObjectID, input: Bool) -> Bool {
        var addr = address(defaultSelector(input: input))
        var value = id
        return AudioObjectSetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil,
                                          UInt32(MemoryLayout<AudioObjectID>.size), &value) == noErr
    }
}
