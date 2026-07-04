// Exits 0 if the default input device is capturing somewhere, 1 if idle.
// Used by speak-reply.sh to stop TTS when the user starts dictating.
import CoreAudio

var addr = AudioObjectPropertyAddress(
    mSelector: kAudioHardwarePropertyDefaultInputDevice,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
)
var deviceID = AudioDeviceID(0)
var size = UInt32(MemoryLayout<AudioDeviceID>.size)
guard AudioObjectGetPropertyData(
    AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &deviceID
) == noErr, deviceID != 0 else { exit(2) }

var runAddr = AudioObjectPropertyAddress(
    mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
)
var running = UInt32(0)
size = UInt32(MemoryLayout<UInt32>.size)
guard AudioObjectGetPropertyData(deviceID, &runAddr, 0, nil, &size, &running) == noErr
else { exit(2) }

exit(running != 0 ? 0 : 1)
