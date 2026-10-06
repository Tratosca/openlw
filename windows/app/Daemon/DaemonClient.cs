// Control channel client: one JSON request per line over the service named pipe (ADR 0007).
// Opens the pipe with exactly the rights granted to authenticated users (read, write data and
// attributes, no pipe instance creation) at Identification level: the service identifies the
// caller and cannot act on its behalf.
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json.Nodes;
using Microsoft.Win32.SafeHandles;

namespace OpenLW.Daemon;

public enum DaemonErrorKind { Unreachable, Decoding, Refused }

public sealed record DaemonError(DaemonErrorKind Kind, string Reason = "")
{
    /// Displayed message: what happened, then what the user can do.
    public string Message => Kind switch
    {
        DaemonErrorKind.Unreachable => "Le service OpenLW ne répond pas. Si le problème persiste, réinstallez OpenLW.",
        DaemonErrorKind.Decoding => "Réponse illisible du service OpenLW. Réinstallez OpenLW pour mettre l'app et le service à la même version.",
        _ => $"Modification impossible : {Reason}",
    };
}

public sealed record DaemonResult(JsonObject? Reply, DaemonError? Error)
{
    public bool Ok => Error is null;
}

public sealed partial class DaemonClient : IDisposable
{
    private const string PipePath = @"\\.\pipe\fr.francois-brille.openlw.daemon";
    private const uint GenericRead = 0x80000000, FileWriteData = 0x2, FileWriteAttributes = 0x100;
    private const uint OpenExisting = 3, SecuritySqosPresent = 0x00100000, SecurityIdentification = 0x00010000;
    private const int ErrorPipeBusy = 231;

    private readonly SemaphoreSlim gate = new(1, 1);
    private FileStream? stream;
    private StreamReader? reader;

    [LibraryImport("kernel32.dll", EntryPoint = "CreateFileW", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    private static partial SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [LibraryImport("kernel32.dll", EntryPoint = "WaitNamedPipeW", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool WaitNamedPipe(string name, uint timeoutMs);

    /// Sends `request`; never throws. Runs off the UI thread.
    public Task<DaemonResult> CallAsync(JsonObject request) => Task.Run(() => Call(request));

    private DaemonResult Call(JsonObject request)
    {
        gate.Wait();
        try
        {
            // One reconnection attempt: the service may have restarted since the last call.
            for (int attempt = 0; attempt < 2; attempt++)
            {
                try
                {
                    Open();
                    byte[] line = Encoding.UTF8.GetBytes(request.ToJsonString() + "\n");
                    stream!.Write(line, 0, line.Length);
                    stream.Flush();
                    string? reply = reader!.ReadLine();
                    if (reply is null)
                    {
                        throw new IOException("connexion fermée");
                    }
                    return Parse(reply);
                }
                catch (IOException)
                {
                    Close();
                }
            }
            return new DaemonResult(null, new DaemonError(DaemonErrorKind.Unreachable));
        }
        finally
        {
            gate.Release();
        }
    }

    private static DaemonResult Parse(string reply)
    {
        if (JsonNode.Parse(reply) is not JsonObject obj)
        {
            return new DaemonResult(null, new DaemonError(DaemonErrorKind.Decoding));
        }
        if (obj["ok"]?.GetValue<bool>() == true)
        {
            return new DaemonResult(obj, null);
        }
        string reason = obj["error"]?.GetValue<string>() ?? "erreur inconnue";
        return new DaemonResult(obj, new DaemonError(DaemonErrorKind.Refused, reason));
    }

    private void Open()
    {
        if (stream is not null)
        {
            return;
        }
        uint access = GenericRead | FileWriteData | FileWriteAttributes;
        uint flags = SecuritySqosPresent | SecurityIdentification;
        for (int attempt = 0; attempt < 3; attempt++)
        {
            SafeFileHandle h = CreateFile(PipePath, access, 0, IntPtr.Zero, OpenExisting, flags, IntPtr.Zero);
            if (!h.IsInvalid)
            {
                stream = new FileStream(h, FileAccess.ReadWrite, 1);
                reader = new StreamReader(stream, new UTF8Encoding(false), false, 4096, leaveOpen: true);
                return;
            }
            int err = Marshal.GetLastPInvokeError();
            h.Dispose();
            if (err != ErrorPipeBusy || !WaitNamedPipe(PipePath, 2000))
            {
                throw new IOException($"tube indisponible ({err})");
            }
        }
        throw new IOException("tube occupé");
    }

    private void Close()
    {
        reader?.Dispose();
        stream?.Dispose();
        reader = null;
        stream = null;
    }

    public void Dispose()
    {
        Close();
        gate.Dispose();
    }
}
