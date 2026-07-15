using System;
using System.Collections.Generic;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Atlas.App.Views;

/// <summary>
/// The remote support-bundle export dialog (PRD §9.18, §18.3). It assembles one
/// redacted, self-contained diagnostic file the user can hand to an IT/support
/// engineer, from data Atlas already has. Modeled on the M8 <see cref="ReportDialog"/>:
/// the user picks the sections to include, a time window (for the incident/change/
/// crash sections), redaction options, and a format, then Generate renders the
/// bundle into a copyable preview that can be saved via a <see cref="FileSavePicker"/>.
///
/// <para>
/// The safety framing is the point of this surface: this file is meant to <b>leave
/// the machine</b>, so redaction is <b>on by default</b> and the dialog states —
/// prominently but calmly — that redaction removes the selected personal details
/// before the file is created, keeping unrelated personal activity off it. After
/// generation the dialog surfaces the reply's <c>redaction_applied</c> echo so the
/// user can <em>see</em> exactly what was stripped.
/// </para>
///
/// <para>
/// The same escape hatches as the report dialog keep this robust under the
/// unpackaged WinUI host: the preview is always copyable, and if the file picker
/// can't be initialized with an owner window we point at Copy rather than fail.
/// Against a service too old to serve GenerateSupportBundle (Unimplemented →
/// Unsupported) the dialog shows a calm "unavailable" bar instead of crashing
/// (task brief §2, §3).
/// </para>
/// </summary>
public sealed partial class SupportBundleDialog : ContentDialog
{
    private readonly string? _who;

    private string _generatedContent = string.Empty;
    private ReportFormat _generatedFormat = ReportFormat.ReportHtml;
    private string _suggestedFilename = string.Empty;

    /// <param name="who">Pipe discriminator (null = default).</param>
    public SupportBundleDialog(string? who)
    {
        _who = who;

        InitializeComponent();

        // Default the time window to 72 h (the proto's default) and prime the
        // redaction summary from the default-on checkboxes.
        WindowChoices.SelectedIndex = 1;
        UpdateRedactionSummary();
    }

    private ReportFormat SelectedFormat()
    {
        var tag = (FormatCombo.SelectedItem as ComboBoxItem)?.Tag as string;
        return tag switch
        {
            "html" => ReportFormat.ReportHtml,
            "json" => ReportFormat.ReportJson,
            "text" => ReportFormat.ReportText,
            _ => ReportFormat.ReportHtml,
        };
    }

    private int SelectedWindowHours()
    {
        var tag = (WindowChoices.SelectedItem as RadioButton)?.Tag as string;
        return tag switch
        {
            "24" => 24,
            "72" => 72,
            "168" => 168,
            _ => 72,
        };
    }

    private RedactionOptions BuildRedaction() => new()
    {
        RedactUserNames = RedactUserNames.IsChecked == true,
        RedactComputerName = RedactComputerName.IsChecked == true,
        RedactPaths = RedactPaths.IsChecked == true,
        RedactCommandLines = RedactCommandLines.IsChecked == true,
    };

    private List<SupportBundleSection> SelectedSections()
    {
        var sections = new List<SupportBundleSection>();
        if (SectionDeviceInfo.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleDeviceInfo);
        }
        if (SectionHealth.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleHealth);
        }
        if (SectionIncidents.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleIncidents);
        }
        if (SectionSystemChanges.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleSystemChanges);
        }
        if (SectionCrashes.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleCrashes);
        }
        if (SectionServices.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleServices);
        }
        if (SectionStartup.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleStartup);
        }
        if (SectionSelfMetrics.IsChecked == true)
        {
            sections.Add(SupportBundleSection.BundleSelfMetrics);
        }
        return sections;
    }

    private void UpdateRedactionSummary()
    {
        // RedactionSummaryText may not exist yet during InitializeComponent wiring.
        if (RedactionSummaryText is not null)
        {
            RedactionSummaryText.Text = SupportBundleFormatter.RedactionSummary(BuildRedaction());
        }
    }

    private void OnRedactionChanged(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        UpdateRedactionSummary();

    private void OnFormatChanged(object sender, SelectionChangedEventArgs e) =>
        InvalidatePreview();

    private void OnWindowChanged(object sender, SelectionChangedEventArgs e) =>
        InvalidatePreview();

    private void InvalidatePreview()
    {
        // Any input change invalidates a prior preview.
        if (PreviewPanel is not null)
        {
            PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;
        }
    }

    private async void OnGenerateClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        StatusBar.IsOpen = false;
        PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;

        var sections = SelectedSections();
        if (sections.Count == 0)
        {
            ShowStatus(InfoBarSeverity.Warning, "Nothing selected",
                "Pick at least one section to include in the bundle.");
            return;
        }

        SetBusy(true);

        var format = SelectedFormat();
        var redaction = BuildRedaction();
        var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var fromMs = SupportBundleFormatter.WindowFromMs(now, SelectedWindowHours());

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .GenerateSupportBundleAsync(fromMs, now, format, redaction, sections)
                .ConfigureAwait(true);

            SetBusy(false);

            if (!outcome.Supported)
            {
                ShowStatus(InfoBarSeverity.Warning, "Support bundle unavailable",
                    "The connected service is too old to create a support bundle. Update the service and try again.");
                return;
            }

            var reply = outcome.Value;
            _generatedContent = reply.Content ?? string.Empty;
            _generatedFormat = format;
            _suggestedFilename = reply.Filename ?? string.Empty;

            PreviewBox.Text = _generatedContent.Length == 0
                ? "(The service returned an empty bundle.)"
                : _generatedContent;

            // Show the user exactly what was stripped (the redaction_applied echo).
            RedactionAppliedBar.Message =
                SupportBundleFormatter.RedactionAppliedSummary(reply.RedactionApplied);
            RedactionAppliedBar.Severity = reply.RedactionApplied.Count > 0
                ? InfoBarSeverity.Success
                : InfoBarSeverity.Informational;
            RedactionAppliedBar.IsOpen = true;

            PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Visible;

            ShowStatus(InfoBarSeverity.Success, "Bundle generated",
                $"{M8Formatter.ReportFormatLabel(format)} support bundle ready — save it to a file or copy it.");
        }
        catch (Exception ex)
        {
            SetBusy(false);
            ShowStatus(InfoBarSeverity.Error, "Could not create the support bundle", ex.Message);
        }
    }

    private async void OnSaveClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (_generatedContent.Length == 0)
        {
            return;
        }

        try
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
                SuggestedFileName = SuggestedFileName(),
            };
            var label = M8Formatter.ReportFormatLabel(_generatedFormat);
            var ext = "." + M8Formatter.ReportFormatExtension(_generatedFormat);
            picker.FileTypeChoices.Add(label, new[] { ext });

            // Unpackaged WinUI: the picker needs an owner HWND or it throws.
            var window = App.MainWindow;
            if (window is null)
            {
                ShowStatus(InfoBarSeverity.Warning, "Can't open the file dialog",
                    "Use Copy instead and paste the bundle into a file.");
                return;
            }
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSaveFileAsync();
            if (file is null)
            {
                return; // user cancelled
            }

            await FileIO.WriteTextAsync(file, _generatedContent);
            ShowStatus(InfoBarSeverity.Success, "Saved", $"Support bundle saved to {file.Name}.");
        }
        catch (Exception ex)
        {
            // Picker fiddliness under the current host: fall back to copy (brief §3).
            ShowStatus(InfoBarSeverity.Warning, "Couldn't save to a file",
                $"{ex.Message} — use Copy instead and paste the bundle into a file.");
        }
    }

    private void OnCopyClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (_generatedContent.Length == 0)
        {
            return;
        }
        var package = new DataPackage();
        package.SetText(_generatedContent);
        Clipboard.SetContent(package);
        ShowStatus(InfoBarSeverity.Success, "Copied", "The support bundle is on the clipboard.");
    }

    private string SuggestedFileName()
    {
        // Prefer the server's suggested name; fall back to a stamped default.
        if (!string.IsNullOrWhiteSpace(_suggestedFilename))
        {
            return _suggestedFilename;
        }
        var stamp = DateTimeOffset.Now.ToString("yyyyMMdd-HHmm");
        return $"atlas-support-{stamp}.{M8Formatter.ReportFormatExtension(_generatedFormat)}";
    }

    private void SetBusy(bool busy)
    {
        BusyRing.IsActive = busy;
        GenerateButton.IsEnabled = !busy;
    }

    private void ShowStatus(InfoBarSeverity severity, string title, string message)
    {
        StatusBar.Severity = severity;
        StatusBar.Title = title;
        StatusBar.Message = message;
        StatusBar.IsOpen = true;
    }
}
