// Headphone preview without the service: the app joins the source multicast group on the
// Livewire interface, decodes RTP L24/L16 (big-endian) and plays the first two channels on the
// default Windows output (shared WASAPI). Same behavior as macos/app/Sources/Listener.swift.
using System.Net;
using System.Net.Sockets;
using NAudio.CoreAudioApi;
using NAudio.Wave;

namespace OpenLW.Audio;

public sealed class ListenError(string message) : Exception(message);

public sealed class Listener : IDisposable
{
    private const int Rate = 48_000;
    /// Buffer before playback (30 ms), and maximum before discarding the excess (200 ms, sender drift).
    private static readonly TimeSpan Target = TimeSpan.FromMilliseconds(30), High = TimeSpan.FromMilliseconds(200);

    private readonly object gate = new();
    private int generation;
    private float peak;
    private WasapiOut? output;
    private BufferedWaveProvider? buffer;

    /// Peak (dBFS) since the last call, null for silence.
    public double? TakePeak()
    {
        lock (gate)
        {
            float p = peak;
            peak = 0;
            return p > 0 ? 20 * Math.Log10(p) : null;
        }
    }

    /// Starts the preview of `group:port` received on the interface with address `ifaceIp`.
    /// `channels`: stream channels (two, or eight for surround); only the first two are played.
    public void Start(string group, string ifaceIp, int channels, int bits, int port = 5004)
    {
        Stop();
        Socket sock = Open(group, ifaceIp, port);
        var format = WaveFormat.CreateIeeeFloatWaveFormat(Rate, 2);
        var buf = new BufferedWaveProvider(format)
        {
            BufferDuration = TimeSpan.FromMilliseconds(500),
            DiscardOnBufferOverflow = true,
            ReadFully = true,
        };
        WasapiOut wo;
        try
        {
            wo = new WasapiOut(AudioClientShareMode.Shared, true, 30);
            wo.Init(buf);
        }
        catch (Exception e)
        {
            sock.Dispose();
            throw new ListenError($"Cannot listen to the source: Windows audio output unavailable ({e.Message}).");
        }
        int gen;
        lock (gate)
        {
            generation++;
            gen = generation;
            output = wo;
            buffer = buf;
        }
        var t = new Thread(() => Receive(sock, channels, bits, gen, wo, buf)) { IsBackground = true, Name = "preview" };
        t.Start();
    }

    public void Stop()
    {
        WasapiOut? wo;
        lock (gate)
        {
            generation++;
            wo = output;
            output = null;
            buffer = null;
        }
        wo?.Stop();
        wo?.Dispose();
    }

    public void Dispose() => Stop();

    private static Socket Open(string group, string ifaceIp, int port)
    {
        var s = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        string step = "socket";
        try
        {
            // Shared port (OpenLW service, other Livewire software); Windows refuses to bind a
            // multicast address, the socket only receives the groups it joined.
            s.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);
            step = "bind";
            s.Bind(new IPEndPoint(IPAddress.Any, port));
            step = "group join";
            s.SetSocketOption(SocketOptionLevel.IP, SocketOptionName.AddMembership,
                new MulticastOption(IPAddress.Parse(group), IPAddress.Parse(ifaceIp)));
            s.ReceiveTimeout = 200;
            return s;
        }
        catch (SocketException e)
        {
            s.Dispose();
            throw new ListenError($"Cannot listen to the source ({step}: {e.Message}). Check the selected Livewire interface.");
        }
    }

    private bool Current(int gen)
    {
        lock (gate)
        {
            return gen == generation;
        }
    }

    private void Receive(Socket sock, int channels, int bits, int gen, WasapiOut wo, BufferedWaveProvider buf)
    {
        using (sock)
        {
            var packet = new byte[2048];
            var frames = new float[2 * 512];
            var bytes = new byte[frames.Length * 4];
            int width = bits / 8, stride = Math.Max(1, channels) * width;
            bool playing = false;
            while (Current(gen))
            {
                int n;
                try
                {
                    n = sock.Receive(packet);
                }
                catch (SocketException)
                {
                    continue; // timeout: check for stop
                }
                if (n < 12 || packet[0] >> 6 != 2)
                {
                    continue;
                }
                // RTP header: 12 bytes, CSRCs, optional extension.
                int off = 12 + 4 * (packet[0] & 0x0F);
                if ((packet[0] & 0x10) != 0 && off + 4 <= n)
                {
                    off += 4 + 4 * (packet[off + 2] << 8 | packet[off + 3]);
                }
                if (off >= n)
                {
                    continue;
                }
                int count = Math.Min((n - off) / stride, 512);
                float p = 0;
                for (int f = 0; f < count; f++)
                {
                    for (int c = 0; c < 2; c++)
                    {
                        int i = off + f * stride + Math.Min(c, channels - 1) * width;
                        float v = width == 3
                            ? (packet[i] << 24 | packet[i + 1] << 16 | packet[i + 2] << 8) / 2147483648f
                            : (short)(packet[i] << 8 | packet[i + 1]) / 32768f;
                        frames[2 * f + c] = v;
                        p = Math.Max(p, Math.Abs(v));
                    }
                }
                Buffer.BlockCopy(frames, 0, bytes, 0, count * 8);
                if (buf.BufferedDuration > High)
                {
                    buf.ClearBuffer();
                }
                buf.AddSamples(bytes, 0, count * 8);
                lock (gate)
                {
                    peak = Math.Max(peak, p);
                }
                if (!playing && buf.BufferedDuration >= Target)
                {
                    wo.Play();
                    playing = true;
                }
            }
        }
    }
}
