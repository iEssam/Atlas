using System;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Atlas.App.Views;

/// <summary>
/// The M8 report-export dialog (PRD §9.18). It lets the user pick a format
/// (HTML/Text/JSON/CSV) and redaction options, makes plain <b>what redaction will
/// strip before generating</b>, then calls GenerateReport. The rendered content
/// is shown in a copyable preview and can be saved via a <see cref="FileSavePicker"/>.
///
/// <para>
/// Two escape hatches keep this robust under the unpackaged WinUI host: the
/// preview is always copyable, and if the file picker can't be initialized with an
/// owner window we surface a note pointing at Copy rather than failing. Against a
/// service too old to serve GenerateReport (Unimplemented → Unsupported) the dialog
/// shows a calm "unavailable" bar instead of crashing (task brief §3, §4).
/// </para>
/// </summary>
public sealed partial class ReportDialog : ContentDialog
{
    private readonly string? _who;
    private readonly long _incidentId;
    private readonly long _fromMs;
    private readonly long _toMs;

    private string _generatedContent = string.Empty;
    private ReportFormat _generatedFormat = ReportFormat.ReportHtml;

    /// <param name="who">Pipe discriminator (null = default).</param>
    /// <param name="incidentId">Incident id, or 0 for the ad-hoc window.</param>
    /// <param name="fromMs">Window start (epoch ms).</param>
    /// <param name="toMs">Window end (epoch ms).</param>
    public ReportDialog(string? who, long incidentId, long fromMs, long toMs)
    {
        _who = who;
        _incidentId = incidentId;
        _fromMs = fromMs;
        _toMs = toMs;

        InitializeComponent();

        UpdateRedactionSummary();
    }

    private ReportFormat SelectedFormat()
    {
        var tag = (FormatCombo.SelectedItem as ComboBoxItem)?.Tag as string;
        return tag switch
        {
            "html" => ReportFormat.ReportHtml,
            "text" => ReportFormat.ReportText,
            "json" => ReportFormat.ReportJson,
            "csv" => ReportFormat.ReportCsv,
            _ => ReportFormat.ReportHtml,
        };
    }

    private RedactionOptions BuildRedaction() => new()
    {
        RedactUserNames = RedactUserNames.IsChecked == true,
        RedactComputerName = RedactComputerName.IsChecked == true,
        RedactPaths = RedactPaths.IsChecked == true,
        RedactCommandLines = RedactCommandLines.IsChecked == true,
    };

    private void UpdateRedactionSummary()
    {
        // RedactionSummaryText may not exist yet during InitializeComponent wiring.
        if (RedactionSummaryText is not null)
        {
            RedactionSummaryText.Text = M8Formatter.RedactionSummary(BuildRedaction());
        }
    }

    private void OnRedactionChanged(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        UpdateRedactionSummary();

    private void OnFormatChanged(object sender, SelectionChangedEventArgs e)
    {
        // A new format invalidates any prior preview.
        if (PreviewPanel is not null)
        {
            PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;
        }
    }

    private async void OnGenerateClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        StatusBar.IsOpen = false;
        PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;
        SetBusy(true);

        var format = SelectedFormat();
        var redaction = BuildRedaction();

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .GenerateReportAsync(_incidentId, _fromMs, _toMs, format, redaction)
                .ConfigureAwait(true);

            SetBusy(false);

            if (!outcome.Supported)
            {
                ShowStatus(InfoBarSeverity.Warning, "Reports unavailable",
                    "The connected service is too old to generate reports. Update the service and try again.");
                return;
            }

            _generatedContent = outcome.Value.Content ?? string.Empty;
            _generatedFormat = format;

            PreviewBox.Text = _generatedContent.Length == 0
                ? "(The service returned an empty report.)"
                : _generatedContent;
            PreviewPanel.Visibility = Microsoft.UI.Xaml.Visibility.Visible;

            ShowStatus(InfoBarSeverity.Success, "Report generated",
                $"{M8Formatter.ReportFormatLabel(format)} report ready — save it to a file or copy it.");
        }
        catch (Exception ex)
        {
            SetBusy(false);
            ShowStatus(InfoBarSeverity.Error, "Could not generate the report", ex.Message);
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
                    "Use Copy instead and paste the report into a file.");
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
            ShowStatus(InfoBarSeverity.Success, "Saved", $"Report saved to {file.Name}.");
        }
        catch (Exception ex)
        {
            // Picker fiddliness under the current host: fall back to copy (brief §3).
            ShowStatus(InfoBarSeverity.Warning, "Couldn't save to a file",
                $"{ex.Message} — use Copy instead and paste the report into a file.");
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
        ShowStatus(InfoBarSeverity.Success, "Copied", "The report is on the clipboard.");
    }

    private string SuggestedFileName()
    {
        var stamp = DateTimeOffset.Now.ToString("yyyyMMdd-HHmm");
        var stem = _incidentId > 0 ? $"atlas-incident-{_incidentId}" : "atlas-diagnosis";
        return $"{stem}-{stamp}.{M8Formatter.ReportFormatExtension(_generatedFormat)}";
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
