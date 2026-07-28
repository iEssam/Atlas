using Atlas.App.Models;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>Hosts a question-based section using native SelectorBar subviews.</summary>
public sealed partial class SectionHostPage : Page
{
    public SectionHostPage()
    {
        InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is not NavigationSection section)
        {
            return;
        }

        SectionSelector.Items.Clear();
        foreach (var destination in section.Destinations)
        {
            SectionSelector.Items.Add(new SelectorBarItem
            {
                Text = destination.Label,
                Tag = destination.PageType,
            });
        }

        if (SectionSelector.Items.Count > 0)
        {
            SectionSelector.SelectedItem = SectionSelector.Items[0];
        }
    }

    private void SectionSelector_SelectionChanged(SelectorBar sender, SelectorBarSelectionChangedEventArgs args)
    {
        if (sender.SelectedItem is SelectorBarItem { Tag: Type pageType }
            && SectionFrame.CurrentSourcePageType != pageType)
        {
            SectionFrame.Navigate(pageType);
        }
    }

    private void BackAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (SectionFrame.CanGoBack)
        {
            SectionFrame.GoBack();
            args.Handled = true;
        }
    }
}
