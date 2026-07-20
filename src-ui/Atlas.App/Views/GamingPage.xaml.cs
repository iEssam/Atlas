using Atlas.App.ViewModels;
using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

public sealed partial class GamingPage : Page
{
    public GamingViewModel ViewModel { get; }

    public GamingPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new GamingViewModel(DispatcherQueue, string.IsNullOrWhiteSpace(who) ? null : who);
        InitializeComponent();
        ViewModel.Trace.CollectionChanged += (_, _) => DispatcherQueue.TryEnqueue(
            () => ProofTrack.SetTrace(ViewModel.Trace, ViewModel.IsRecording));
        ViewModel.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName == nameof(ViewModel.IsRecording))
            {
                DispatcherQueue.TryEnqueue(() => ProofTrack.SetTrace(ViewModel.Trace, ViewModel.IsRecording));
            }
        };
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        _ = ViewModel.LoadAsync();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void Page_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        var wide = e.NewSize.Width >= 1008;
        var medium = !wide && e.NewSize.Width >= 640;
        PageLayout.Padding = wide ? new Thickness(24) : new Thickness(16, 20, 16, 16);

        HeaderObjectiveColumn.Width = wide || medium ? GridLength.Auto : new GridLength(0);
        Grid.SetRow(ObjectiveSelector, wide || medium ? 0 : 1);
        Grid.SetColumn(ObjectiveSelector, wide || medium ? 1 : 0);
        ObjectiveSelector.MinWidth = wide || medium ? 260 : 0;

        GameRail.Visibility = wide ? Visibility.Visible : Visibility.Collapsed;
        CompactGameSelector.Visibility = wide ? Visibility.Collapsed : Visibility.Visible;
        GameRailColumn.Width = wide ? new GridLength(220) : new GridLength(0);
        ProofColumn.Width = new GridLength(1, GridUnitType.Star);
        FindingColumn.Width = wide ? new GridLength(360) : new GridLength(0);

        Grid.SetRow(ProofPane, 0);
        Grid.SetColumn(ProofPane, wide ? 1 : 0);
        Grid.SetRow(FindingsPane, wide ? 0 : 1);
        Grid.SetColumn(FindingsPane, wide ? 2 : 0);
        Grid.SetColumnSpan(ProofPane, 1);
        Grid.SetColumnSpan(FindingsPane, wide ? 1 : 2);
    }

    private void Refresh_Click(object sender, RoutedEventArgs e) => _ = ViewModel.LoadAsync();

    private async void PreviewPlan_Click(object sender, RoutedEventArgs e)
    {
        var plan = await ViewModel.PreviewPlanAsync();
        if (plan is null) return;

        var root = new StackPanel { Spacing = 16, MinWidth = 520 };
        root.Children.Add(new TextBlock
        {
            Text = $"{plan.GameName} · {FriendlyObjective(plan.Objective)}",
            Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
        });
        root.Children.Add(new TextBlock
        {
            Text = "Atlas captured the current configuration and bound this immutable plan to it. Only automatic, reversible actions can be selected here.",
            TextWrapping = TextWrapping.Wrap,
        });
        root.Children.Add(new TextBlock
        {
            Text = $"Plan hash: {plan.PlanHash}",
            IsTextSelectionEnabled = true,
            TextWrapping = TextWrapping.Wrap,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        });

        var checkBoxes = new List<CheckBox>();
        AddLane(root, plan, GamingRiskLane.AutomaticReversible, "Automatic and reversible", checkBoxes);
        AddLane(root, plan, GamingRiskLane.AdvancedExperiment, "Advanced experiments", checkBoxes);
        AddLane(root, plan, GamingRiskLane.GuidedManual, "Guided manual checks", checkBoxes);
        AddLane(root, plan, GamingRiskLane.Blocked, "Blocked or discouraged", checkBoxes);

        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Review the exact gaming plan",
            Content = new ScrollViewer
            {
                MaxHeight = 560,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                Content = root,
            },
            PrimaryButtonText = "Apply selected safe steps",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            IsPrimaryButtonEnabled = checkBoxes.Any(box => box.IsChecked == true),
        };
        foreach (var checkBox in checkBoxes)
        {
            checkBox.Checked += (_, _) => dialog.IsPrimaryButtonEnabled = checkBoxes.Any(box => box.IsChecked == true);
            checkBox.Unchecked += (_, _) => dialog.IsPrimaryButtonEnabled = checkBoxes.Any(box => box.IsChecked == true);
        }

        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        var selected = checkBoxes.Where(box => box.IsChecked == true).Select(box => (string)box.Tag).ToArray();
        await ViewModel.PrepareAndExecuteAsync(selected);
    }

    private static void AddLane(
        Panel root,
        GamingPlan plan,
        GamingRiskLane lane,
        string heading,
        ICollection<CheckBox> checkBoxes)
    {
        var steps = plan.Steps.Where(step => step.RiskLane == lane).ToArray();
        if (steps.Length == 0) return;
        root.Children.Add(new TextBlock
        {
            Text = heading,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });
        foreach (var step in steps)
        {
            var line = new StackPanel { Spacing = 4, Margin = new Thickness(0, 0, 0, 10) };
            if (lane == GamingRiskLane.AutomaticReversible && step.Executable)
            {
                var checkBox = new CheckBox
                {
                    Content = step.Title,
                    IsChecked = step.Selected,
                    Tag = step.Id,
                };
                checkBoxes.Add(checkBox);
                line.Children.Add(checkBox);
            }
            else
            {
                line.Children.Add(new TextBlock { Text = step.Title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
            }
            line.Children.Add(new TextBlock { Text = step.Explanation, TextWrapping = TextWrapping.Wrap });
            line.Children.Add(new TextBlock { Text = $"Before: {step.BeforeValue}\nAfter: {step.AfterValue}", IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            line.Children.Add(new TextBlock
            {
                Text = $"Tradeoff / scope: {step.Scope}\nRestart: {step.RestartRequirement}\nRollback: {step.Rollback}\nVerification: {step.Verification}",
                TextWrapping = TextWrapping.Wrap,
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            });
            root.Children.Add(line);
        }
    }

    private async void Rollback_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Roll back the current gaming plan?",
            Content = "Atlas will restore each recorded before-value in reverse order and verify the result.",
            PrimaryButtonText = "Roll back",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            await ViewModel.RollbackAsync();
        }
    }

    private async void Keep_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Keep the current gaming plan?",
            Content = "Atlas will leave the verified settings in place and cancel automatic session restoration. Exact before-values remain available if you roll back later.",
            PrimaryButtonText = "Keep",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            await ViewModel.KeepAsync();
        }
    }

    private void StartSession_Click(object sender, RoutedEventArgs e) => _ = ViewModel.StartSessionAsync();
    private void StopSession_Click(object sender, RoutedEventArgs e) => _ = ViewModel.StopSessionAsync();

    private async void Session_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (e.AddedItems.FirstOrDefault() is GamingSessionDisplay item)
        {
            await ViewModel.LoadTraceAsync(item.Session);
            ProofTrack.SetTrace(ViewModel.Trace, ViewModel.IsRecording);
        }
    }

    private static string FriendlyObjective(GamingObjective objective) => objective == GamingObjective.SmoothCompetitive
        ? "Smooth competitive"
        : "Competitive latency";
}
