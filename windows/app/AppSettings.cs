// App-only preferences (manually entered channels, advanced settings visibility), stored in
// %LOCALAPPDATA%\OpenLW\app.json. Network settings live in the service configuration.
using System.Text.Json;
using System.Text.Json.Serialization;

namespace OpenLW;

public sealed class ManualSource
{
    [JsonPropertyName("channel")] public int Channel { get; set; }
    [JsonPropertyName("kind")] public string Kind { get; set; } = "stereo";
}

public sealed class AppSettings
{
    [JsonPropertyName("manual")] public List<ManualSource> Manual { get; set; } = [];
    [JsonPropertyName("advanced_visible")] public bool AdvancedVisible { get; set; }

    private static string Path => System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenLW", "app.json");

    public static AppSettings Load()
    {
        try
        {
            return JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(Path)) ?? new AppSettings();
        }
        catch (Exception)
        {
            return new AppSettings();
        }
    }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!);
            File.WriteAllText(Path, JsonSerializer.Serialize(this));
        }
        catch (Exception)
        {
            // Preferences only: losing them is harmless.
        }
    }
}
