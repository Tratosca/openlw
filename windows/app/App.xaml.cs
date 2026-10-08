// OpenLW for Windows: configuration app for the network service (named pipe).
using Microsoft.UI.Xaml;

namespace OpenLW;

public partial class App : Application
{
    private Window? window;

    public App()
    {
        Loc.SelectLanguage();
        InitializeComponent();
        // Unhandled exceptions: written to %LOCALAPPDATA%\OpenLW\crash.log for diagnosis.
        UnhandledException += (_, e) => LogCrash(e.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, e) => LogCrash(e.ExceptionObject as Exception);
    }

    internal static void LogCrash(Exception? e)
    {
        try
        {
            string dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenLW");
            Directory.CreateDirectory(dir);
            File.AppendAllText(Path.Combine(dir, "crash.log"), $"{DateTime.Now:O} {e}{Environment.NewLine}");
        }
        catch (Exception)
        {
            // Nothing more to do while crashing.
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        window = new MainWindow();
        window.Activate();
    }
}
