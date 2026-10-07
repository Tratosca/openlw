// User-facing strings: MRT Core resources (Strings\en-US and Strings\fr-FR Resources.resw,
// compiled into OpenLW.pri next to the executable). XAML uses x:Uid, code uses Loc.S / Loc.F.
// English by default, French when the Windows display language is French.
using System.Globalization;
using Microsoft.Windows.ApplicationModel.Resources;
using Microsoft.Windows.Globalization;

namespace OpenLW;

internal static class Loc
{
    private static ResourceLoader? loader;

    /// Chooses the UI language before any XAML is loaded. OPENLW_UI_LANGUAGE (en-US or fr-FR)
    /// forces it; otherwise French for a French display language, English for any other.
    public static void SelectLanguage()
    {
        string? forced = Environment.GetEnvironmentVariable("OPENLW_UI_LANGUAGE");
        string language = forced is "en-US" or "fr-FR" ? forced
            : CultureInfo.CurrentUICulture.TwoLetterISOLanguageName == "fr" ? "fr-FR" : "en-US";
        // Without the override, MRT Core would also pick French for a user whose display language
        // is another one with French further down the preferred language list.
        ApplicationLanguages.PrimaryLanguageOverride = language;
        CultureInfo.CurrentUICulture = CultureInfo.DefaultThreadCurrentUICulture = new CultureInfo(language);
    }

    /// String `key`; the key itself if the resource is missing (visible, never empty).
    public static string S(string key)
    {
        try
        {
            loader ??= new ResourceLoader();
            string s = loader.GetString(key);
            return s.Length == 0 ? key : s;
        }
        catch (Exception)
        {
            return key;
        }
    }

    /// Format string `key` ({0}, {1}…) applied to `args`.
    public static string F(string key, params object[] args) => string.Format(CultureInfo.CurrentCulture, S(key), args);
}
