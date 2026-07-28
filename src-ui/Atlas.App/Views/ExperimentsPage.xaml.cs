using System;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.Storage.Pickers;

namespace Atlas.App.Views;

public sealed partial class ExperimentsPage : Page
{
    private readonly string? _who;
    private CompareExperimentReply? _comparison;

    public ObservableCollection<Experiment> Experiments { get; } = new();

    public ExperimentsPage()
    {
        _who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        InitializeComponent();
    }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await LoadAsync();
    }

    private async Task LoadAsync(long selectId = 0)
    {
        SetStatus("Loading saved experiments...", InfoBarSeverity.Informational);
        try
        {
            using var channel = AtlasChannel.Connect(string.IsNullOrWhiteSpace(_who) ? null : _who);
            var outcome = await channel.ListExperimentsAsync();
            if (!outcome.Supported)
            {
                SetStatus("Experiments require a newer Atlas service.", InfoBarSeverity.Warning);
                return;
            }
            Experiments.Clear();
            foreach (var item in outcome.Value.Experiments)
            {
                Experiments.Add(item);
            }
            StatusBar.IsOpen = false;
            if (Experiments.Count == 0)
            {
                ShowEmpty();
                return;
            }
            ExperimentList.SelectedItem = selectId == 0
                ? Experiments[0]
                : Experiments.FirstOrDefault(item => item.Id == selectId) ?? Experiments[0];
        }
        catch (Exception ex)
        {
            SetStatus($"Could not load experiments: {ex.Message}", InfoBarSeverity.Error);
        }
    }

    private async void NewExperiment_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ExperimentEditDialog { XamlRoot = XamlRoot };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary || dialog.Experiment is null)
        {
            return;
        }
        try
        {
            SetStatus("Creating experiment...", InfoBarSeverity.Informational);
            using var channel = AtlasChannel.Connect(string.IsNullOrWhiteSpace(_who) ? null : _who);
            var outcome = await channel.CreateExperimentAsync(dialog.Experiment);
            if (!outcome.Supported)
            {
                SetStatus("Experiments require a newer Atlas service.", InfoBarSeverity.Warning);
                return;
            }
            await LoadAsync(outcome.Value.Experiment.Id);
        }
        catch (Exception ex)
        {
            SetStatus($"Could not create the experiment: {ex.Message}", InfoBarSeverity.Error);
        }
    }

    private void Refresh_Click(object sender, RoutedEventArgs e) => _ = LoadAsync(
        (ExperimentList.SelectedItem as Experiment)?.Id ?? 0);

    private async void ExperimentList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (ExperimentList.SelectedItem is Experiment selected)
        {
            await CompareAsync(selected.Id);
        }
        else
        {
            ShowEmpty();
        }
    }

    private async Task CompareAsync(long id)
    {
        SetStatus("Comparing retained evidence...", InfoBarSeverity.Informational);
        ExportButton.IsEnabled = false;
        try
        {
            using var channel = AtlasChannel.Connect(string.IsNullOrWhiteSpace(_who) ? null : _who);
            var outcome = await channel.CompareExperimentAsync(id);
            if (!outcome.Supported)
            {
                SetStatus("Experiments require a newer Atlas service.", InfoBarSeverity.Warning);
                return;
            }
            _comparison = outcome.Value;
            Render(outcome.Value);
            StatusBar.IsOpen = false;
            ExportButton.IsEnabled = true;
        }
        catch (Exception ex)
        {
            SetStatus($"Could not compare the experiment: {ex.Message}", InfoBarSeverity.Error);
        }
    }

    private void Render(CompareExperimentReply result)
    {
        var experiment = result.Experiment;
        var baseline = result.Baseline;
        var followup = result.Followup;
        EmptyState.Visibility = Visibility.Collapsed;
        ResultPanel.Visibility = Visibility.Visible;
        ResultName.Text = experiment.Name;
        ResultChange.Text = experiment.ChangeDescription;
        VerdictBar.Title = VerdictTitle(result.Verdict);
        VerdictBar.Message = $"{result.Summary} Average delta: {result.AverageDeltaPercent:+0.0;-0.0;0.0}%.";
        VerdictBar.Severity = result.Verdict switch
        {
            ExperimentVerdict.ExperimentImproved => InfoBarSeverity.Success,
            ExperimentVerdict.ExperimentRegressed => InfoBarSeverity.Warning,
            ExperimentVerdict.ExperimentInsufficientData => InfoBarSeverity.Warning,
            _ => InfoBarSeverity.Informational,
        };
        BaselineAverage.Text = FormatMetric(baseline.Average);
        FollowupAverage.Text = FormatMetric(followup.Average);
        BaselinePeak.Text = FormatMetric(baseline.Peak);
        FollowupPeak.Text = FormatMetric(followup.Peak);
        BaselineAbove.Text = FormatDuration(baseline.DurationAboveThresholdMs);
        FollowupAbove.Text = FormatDuration(followup.DurationAboveThresholdMs);
        BaselineCrashes.Text = baseline.Crashes.ToString();
        FollowupCrashes.Text = followup.Crashes.ToString();
        BaselineChanges.Text = baseline.SystemChanges.ToString();
        FollowupChanges.Text = followup.SystemChanges.ToString();
        NewProcesses.Text = result.NewProcesses.Count == 0
            ? "New in follow-up: none observed"
            : $"New in follow-up: {string.Join(", ", result.NewProcesses)}";
        RemovedProcesses.Text = result.RemovedProcesses.Count == 0
            ? "Only in baseline: none observed"
            : $"Only in baseline: {string.Join(", ", result.RemovedProcesses)}";
        QualityText.Text = result.DataQuality;
        CaveatText.Text = result.Caveat;
    }

    private async void Export_Click(object sender, RoutedEventArgs e)
    {
        if (_comparison is null || App.MainWindow is null)
        {
            return;
        }
        var picker = new FileSavePicker { SuggestedFileName = "system-atlas-experiment" };
        picker.FileTypeChoices.Add("Text report", new[] { ".txt" });
        WinRT.Interop.InitializeWithWindow.Initialize(
            picker, WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow));
        var file = await picker.PickSaveFileAsync();
        if (file is null)
        {
            return;
        }
        await File.WriteAllTextAsync(file.Path, BuildReport(_comparison));
        SetStatus($"Saved {file.Name}", InfoBarSeverity.Success);
    }

    private static string BuildReport(CompareExperimentReply result)
    {
        var text = new StringBuilder();
        text.AppendLine("Atlas before-and-after experiment");
        text.AppendLine(result.Experiment.Name);
        text.AppendLine($"Change: {result.Experiment.ChangeDescription}");
        text.AppendLine($"Verdict: {VerdictTitle(result.Verdict)}");
        text.AppendLine(result.Summary);
        text.AppendLine($"Average delta: {result.AverageDeltaPercent:+0.0;-0.0;0.0}%");
        text.AppendLine($"Baseline average / peak: {FormatMetric(result.Baseline.Average)} / {FormatMetric(result.Baseline.Peak)}");
        text.AppendLine($"Follow-up average / peak: {FormatMetric(result.Followup.Average)} / {FormatMetric(result.Followup.Peak)}");
        text.AppendLine($"New processes: {string.Join(", ", result.NewProcesses)}");
        text.AppendLine($"Only in baseline: {string.Join(", ", result.RemovedProcesses)}");
        text.AppendLine($"Data quality: {result.DataQuality}");
        text.AppendLine(result.Caveat);
        return text.ToString();
    }

    private void ShowEmpty()
    {
        _comparison = null;
        ExportButton.IsEnabled = false;
        EmptyState.Visibility = Visibility.Visible;
        ResultPanel.Visibility = Visibility.Collapsed;
    }

    private void SetStatus(string message, InfoBarSeverity severity)
    {
        StatusBar.Title = severity == InfoBarSeverity.Error ? "Experiment error" : "Experiments";
        StatusBar.Message = message;
        StatusBar.Severity = severity;
        StatusBar.IsOpen = true;
    }

    private static string VerdictTitle(ExperimentVerdict verdict) => verdict switch
    {
        ExperimentVerdict.ExperimentImproved => "Measured improvement",
        ExperimentVerdict.ExperimentRegressed => "Measured regression",
        ExperimentVerdict.ExperimentNoClearChange => "No clear change",
        ExperimentVerdict.ExperimentInsufficientData => "Insufficient data",
        _ => "Comparison unavailable",
    };

    private static string FormatMetric(double raw) => $"{raw / 10.0:0.0}%";
    private static string FormatDuration(ulong milliseconds) => TimeSpan.FromMilliseconds(milliseconds).TotalMinutes < 1
        ? $"{TimeSpan.FromMilliseconds(milliseconds).TotalSeconds:0} sec"
        : $"{TimeSpan.FromMilliseconds(milliseconds).TotalMinutes:0.0} min";
}
