// Livewire source preview: app receives multicast RTP directly on the Livewire
// interface and plays it on the Mac's default output. Daemon patch remains unchanged.
// Daemon opens port 5004 with SO_REUSEPORT: both receive a copy of each packet.

import AudioToolbox
import Darwin
import Foundation

/// Livewire channel multicast group (docs/protocol/01-channels.md).
func livewireGroup(channel: Int, kind: String) -> String {
    let base: Int
    switch kind {
    case "backfeed": base = 193
    case "surround": base = 196
    default: base = 192
    }
    return "239.\(base).\(channel >> 8).\(channel & 0xFF)"
}

final class Listener {
    /// Livewire network samples per second.
    static let rate = 48_000.0
    private static let target = 1_440 // 30 ms buffer before playback
    private static let high = 9_600 // Above 200 ms, discard excess (transmitter drift)

    private var lock = os_unfair_lock()
    private var ring = [Float](repeating: 0, count: 2 * 48_000)
    private var readPos = 0
    private var writePos = 0
    private var primed = false
    private var peak: Float = 0

    /// Preview generation: receive thread stops as soon as it changes.
    private var generation = 0
    private var thread: Thread?
    private var queue: AudioQueueRef?

    deinit { stop() }

    /// Peak (dBFS) since last call, nil for silence.
    func takePeak() -> Double? {
        os_unfair_lock_lock(&lock)
        let p = peak
        peak = 0
        os_unfair_lock_unlock(&lock)
        return p > 0 ? 20 * log10(Double(p)) : nil
    }

    /// Start preview of `group:port` received on `iface` (BSD name, IPv4 address).
    /// `channels`: stream channels (two, or eight for surround); play only first two.
    func start(group: String, port: UInt16 = 5004, iface: String, ifaceIP: String, channels: Int, bits: Int) throws {
        stop()
        let sock = try Self.openSocket(group: group, port: port, iface: iface, ifaceIP: ifaceIP)
        os_unfair_lock_lock(&lock)
        readPos = 0
        writePos = 0
        primed = false
        generation += 1
        let gen = generation
        os_unfair_lock_unlock(&lock)
        let t = Thread { [weak self] in self?.receive(sock: sock, channels: channels, bits: bits, generation: gen) }
        t.name = "preview"
        t.qualityOfService = .userInteractive
        thread = t
        t.start()
        try startQueue()
    }

    func stop() {
        os_unfair_lock_lock(&lock)
        generation += 1
        os_unfair_lock_unlock(&lock)
        thread = nil
        if let q = queue {
            AudioQueueStop(q, true)
            AudioQueueDispose(q, true)
            queue = nil
        }
    }

    // MARK: - Network

    private static func openSocket(group: String, port: UInt16, iface: String, ifaceIP: String) throws -> Int32 {
        let s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        guard s >= 0 else { throw ListenError.socket("socket", errno) }
        var one: Int32 = 1
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(s, SOL_SOCKET, SO_REUSEPORT, &one, socklen_t(MemoryLayout<Int32>.size))
        var index = if_nametoindex(iface)
        if index != 0 {
            setsockopt(s, IPPROTO_IP, IP_BOUND_IF, &index, socklen_t(MemoryLayout<UInt32>.size))
        }
        var timeout = timeval(tv_sec: 0, tv_usec: 200_000)
        setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        // Bind group address: only packets for this group reach the socket.
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr.s_addr = inet_addr(group)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0 else { close(s); throw ListenError.socket("bind", errno) }
        var mreq = ip_mreq(imr_multiaddr: in_addr(s_addr: inet_addr(group)), imr_interface: in_addr(s_addr: inet_addr(ifaceIP)))
        guard setsockopt(s, IPPROTO_IP, IP_ADD_MEMBERSHIP, &mreq, socklen_t(MemoryLayout<ip_mreq>.size)) == 0 else {
            close(s)
            throw ListenError.socket("group membership", errno)
        }
        return s
    }

    private func current(_ gen: Int) -> Bool {
        os_unfair_lock_lock(&lock)
        defer { os_unfair_lock_unlock(&lock) }
        return gen == generation
    }

    private func receive(sock: Int32, channels: Int, bits: Int, generation gen: Int) {
        defer { close(sock) }
        var buf = [UInt8](repeating: 0, count: 2048)
        var frame = [Float](repeating: 0, count: 2 * 512)
        let bytes = bits / 8
        while current(gen) {
            let n = recv(sock, &buf, buf.count, 0)
            guard n >= 12, buf[0] >> 6 == 2 else { continue }
            // RTP header: 12 bytes, CSRCs, optional extension.
            var off = 12 + 4 * Int(buf[0] & 0x0F)
            if buf[0] & 0x10 != 0, off + 4 <= n {
                off += 4 + 4 * (Int(buf[off + 2]) << 8 | Int(buf[off + 3]))
            }
            let stride = channels * bytes
            guard off < n, stride > 0 else { continue }
            let frames = min((n - off) / stride, 512)
            var p: Float = 0
            for f in 0..<frames {
                for c in 0..<2 {
                    let i = off + f * stride + min(c, channels - 1) * bytes
                    let v: Float
                    if bytes == 3 {
                        let raw = Int32(bitPattern: UInt32(buf[i]) << 24 | UInt32(buf[i + 1]) << 16 | UInt32(buf[i + 2]) << 8) >> 8
                        v = Float(raw) / 8_388_608
                    } else {
                        v = Float(Int16(bitPattern: UInt16(buf[i]) << 8 | UInt16(buf[i + 1]))) / 32_768
                    }
                    frame[2 * f + c] = v
                    p = max(p, abs(v))
                }
            }
            push(frame, frames: frames, peak: p)
        }
    }

    private func push(_ data: [Float], frames: Int, peak p: Float) {
        os_unfair_lock_lock(&lock)
        let cap = ring.count / 2
        let free = cap - (writePos - readPos)
        let n = min(frames, free)
        for f in 0..<n {
            let slot = (writePos + f) % cap
            ring[2 * slot] = data[2 * f]
            ring[2 * slot + 1] = data[2 * f + 1]
        }
        writePos += n
        peak = max(peak, p)
        os_unfair_lock_unlock(&lock)
    }

    /// Fill `out` (interleaved stereo): silence before priming, slip if excessive backlog.
    fileprivate func pull(_ out: UnsafeMutablePointer<Float>, frames: Int) {
        os_unfair_lock_lock(&lock)
        defer { os_unfair_lock_unlock(&lock) }
        let cap = ring.count / 2
        var avail = writePos - readPos
        if !primed {
            primed = avail >= Self.target
        }
        if avail > Self.high {
            readPos += avail - Self.target
            avail = Self.target
        }
        let n = primed ? min(frames, avail) : 0
        for f in 0..<frames {
            if f < n {
                let slot = (readPos + f) % cap
                out[2 * f] = ring[2 * slot]
                out[2 * f + 1] = ring[2 * slot + 1]
            } else {
                out[2 * f] = 0
                out[2 * f + 1] = 0
            }
        }
        readPos += n
        if primed && n < frames {
            primed = false
        }
    }

    // MARK: - Audio output (AudioQueue: available on 10.13)

    private func startQueue() throws {
        var format = AudioStreamBasicDescription(
            mSampleRate: Self.rate, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 8, mFramesPerPacket: 1, mBytesPerFrame: 8, mChannelsPerFrame: 2,
            mBitsPerChannel: 32, mReserved: 0)
        var q: AudioQueueRef?
        let me = Unmanaged.passUnretained(self).toOpaque()
        var status = AudioQueueNewOutput(&format, { user, queue, buffer in
            guard let user = user else { return }
            let listener = Unmanaged<Listener>.fromOpaque(user).takeUnretainedValue()
            let frames = Int(buffer.pointee.mAudioDataBytesCapacity) / 8
            listener.pull(buffer.pointee.mAudioData.assumingMemoryBound(to: Float.self), frames: frames)
            buffer.pointee.mAudioDataByteSize = UInt32(frames * 8)
            AudioQueueEnqueueBuffer(queue, buffer, 0, nil)
        }, me, nil, nil, 0, &q)
        guard status == noErr, let queue = q else { throw ListenError.audio(status) }
        self.queue = queue
        for _ in 0..<3 {
            var buffer: AudioQueueBufferRef?
            status = AudioQueueAllocateBuffer(queue, 480 * 8, &buffer) // 10 ms
            guard status == noErr, let b = buffer else { throw ListenError.audio(status) }
            pull(b.pointee.mAudioData.assumingMemoryBound(to: Float.self), frames: 480)
            b.pointee.mAudioDataByteSize = 480 * 8
            AudioQueueEnqueueBuffer(queue, b, 0, nil)
        }
        status = AudioQueueStart(queue, nil)
        guard status == noErr else { throw ListenError.audio(status) }
    }
}

enum ListenError: Error {
    case socket(String, Int32)
    case audio(OSStatus)

    var message: String {
        switch self {
        case .socket(let step, let code):
            return "Cannot listen to the source (\(step): \(String(cString: strerror(code)))). Check the selected Livewire interface."
        case .audio(let status):
            return "Cannot listen to the source: the Mac's audio output is unavailable (error \(status))."
        }
    }
}
