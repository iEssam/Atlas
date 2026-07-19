using System;
using Atlas.V0;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

public sealed partial class ExperimentEditDialog : ContentDialog
{
    public Experiment? Experiment { get; private set; }

    public ExperimentEditDialog()
    {
        InitializeComponent();
        var now = DateTimeOffset.Now;
        SetPeriod(BaselineFromDate, BaselineFromTime, now.AddHours(-2));
        SetPeriod(BaselineToDate, BaselineToTime, now.AddHours(-1));
        SetPeriod(FollowupFromDate, FollowupFromTime, now.AddHours(-1));
        SetPeriod(FollowupToDate, FollowupToTime, now);
    }

    private void PrimaryButton_Click(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        var baselineFrom = Read(BaselineFromDate, BaselineFromTime);
        var baselineTo = Read(BaselineToDate, BaselineToTime);
        var followupFrom = Read(FollowupFromDate, FollowupFromTime);
        var followupTo = Read(FollowupToDate, FollowupToTime);
        var name = NameBox.Text.Trim();
        var change = ChangeBox.Text.Trim();
        if (name.Length == 0 || change.Length == 0 || baselineFrom >= baselineTo || followupFrom >= followupTo)
        {
            args.Cancel = true;
            ValidationBar.Message = "Enter a name and change, and make sure each From time is earlier than its To time.";
            ValidationBar.IsOpen = true;
            return;
        }

        var metric = int.Parse(((ComboBoxItem)MetricBox.SelectedItem).Tag.ToString()!);
        Experiment = new Experiment
        {
            Name = name,
            ChangeDescription = change,
            Metric = (MetricKind)metric,
            Threshold = ThresholdBox.Value * 10.0,
            Baseline = new TimeRange
            {
                FromMs = baselineFrom.ToUniversalTime().ToUnixTimeMilliseconds(),
                ToMs = baselineTo.ToUniversalTime().ToUnixTimeMilliseconds(),
            },
            Followup = new TimeRange
            {
                FromMs = followupFrom.ToUniversalTime().ToUnixTimeMilliseconds(),
                ToMs = followupTo.ToUniversalTime().ToUnixTimeMilliseconds(),
            },
        };
    }

    private static void SetPeriod(DatePicker date, TimePicker time, DateTimeOffset value)
    {
        date.Date = value;
        time.Time = value.TimeOfDay;
    }

    private static DateTimeOffset Read(DatePicker date, TimePicker time)
    {
        var day = date.Date.Date;
        var clock = time.Time;
        return new DateTimeOffset(day.Year, day.Month, day.Day, clock.Hours, clock.Minutes, 0, TimeZoneInfo.Local.GetUtcOffset(day));
    }
}
