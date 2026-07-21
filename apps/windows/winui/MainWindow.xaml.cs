using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.UI;

namespace DontSpeak;

/// <summary>Fluent Agents/Status/Tools/Logs/Credits. Status push-driven by App; close hides to tray.</summary>
public sealed partial class MainWindow : Window
{
    // Orange = Brand.Warning (warming/download); macOS parity.
    private static readonly SolidColorBrush Green = new(Color.FromArgb(255, 46, 160, 67));
    private static readonly SolidColorBrush Orange = new(Brand.Warning);
    private static readonly SolidColorBrush Red = new(Color.FromArgb(255, 232, 70, 70));
    private static readonly SolidColorBrush Gray = new(Color.FromArgb(120, 150, 150, 155));
    private static readonly FontFamily Mono = new("Cascadia Mono, Consolas");
    private static readonly bool DiarizationUiEnabled = Native.DiarizationUiEnabled();

    public MainWindow()
    {
        InitializeComponent();
        UsageList.ChildrenTransitions = new TransitionCollection { new RepositionThemeTransition() };
        AppWindow.Resize(new Windows.Graphics.SizeInt32(380, 620));
        var icoPath = System.IO.Path.Combine(AppContext.BaseDirectory, "AppIcon.ico");
        if (System.IO.File.Exists(icoPath)) AppWindow.SetIcon(icoPath);
        // Width-resizable; height locked to Status content.
        // IsMaximizable/IsMinimizable=false greys buttons — StripMinMaxButtons drops WS_*BOX.
        if (AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter pr)
        {
            pr.IsResizable = true;
            pr.IsMaximizable = false;
            pr.IsMinimizable = false;
            pr.PreferredMinimumWidth = 380;
            pr.PreferredMinimumHeight = 240;
        }
        StripMinMaxButtons();
        HookTitleBarTheme();
        Nav.Loaded += (_, _) => SizeStateStripe();
        Nav.SizeChanged += (_, _) => SizeStateStripe();
        LoadTools();
        LoadLibraries();
        RefreshStatus();

        // App push calls ApplyPushed. One-shot on show; pushes no-op while hidden.
        AppWindow.Changed += (s, e) =>
        {
            if (e.DidVisibilityChange && s.IsVisible)
            {
                RefreshStatus();
                // After arrange so ActualHeight is valid.
                DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, CapHeightToStatusContent);
            }
        };

        // SizeChanged is post-arrange — manual Measure blanks the window.
        if (StatusScroll?.Content is FrameworkElement statusPanel)
            statusPanel.SizeChanged += (_, _) => CapHeightToStatusContent();
    }

    // WinUI 3 title bar ignores system theme by default.
    private void HookTitleBarTheme()
    {
        if (Content is not FrameworkElement root) return;
        // ActualTheme wrong until Loaded (reads Light under system dark).
        ApplyTitleBarTheme(root.ActualTheme);
        root.Loaded += (_, _) => ApplyTitleBarTheme(root.ActualTheme);
        root.ActualThemeChanged += (s, _) => ApplyTitleBarTheme(s.ActualTheme);
    }

    private void ApplyTitleBarTheme(ElementTheme theme)
    {
        if (!Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported()) return;
        var tb = AppWindow.TitleBar;
        bool dark = theme == ElementTheme.Dark;
        Color bg = dark ? Color.FromArgb(255, 32, 32, 32) : Color.FromArgb(255, 243, 243, 243);
        Color fg = dark ? Colors.White : Colors.Black;
        Color inactiveFg = dark ? Color.FromArgb(255, 150, 150, 150) : Color.FromArgb(255, 120, 120, 120);
        Color hover = dark ? Color.FromArgb(255, 55, 55, 55) : Color.FromArgb(255, 225, 225, 225);
        Color pressed = dark ? Color.FromArgb(255, 70, 70, 70) : Color.FromArgb(255, 210, 210, 210);
        tb.BackgroundColor = bg;
        tb.ForegroundColor = fg;
        tb.InactiveBackgroundColor = bg;
        tb.InactiveForegroundColor = inactiveFg;
        tb.ButtonBackgroundColor = bg;
        tb.ButtonForegroundColor = fg;
        tb.ButtonInactiveBackgroundColor = bg;
        tb.ButtonInactiveForegroundColor = inactiveFg;
        tb.ButtonHoverBackgroundColor = hover;
        tb.ButtonHoverForegroundColor = fg;
        tb.ButtonPressedBackgroundColor = pressed;
        tb.ButtonPressedForegroundColor = fg;
    }

    // Drop WS_MINIMIZEBOX/MAXIMIZEBOX; keep WS_THICKFRAME for width resize.
    private void StripMinMaxButtons()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        long style = GetWindowLongPtr(hwnd, GWL_STYLE).ToInt64();
        // checked: CA2020
        SetWindowLongPtr(hwnd, GWL_STYLE, checked((IntPtr)(style & ~(WS_MINIMIZEBOX | WS_MAXIMIZEBOX))));
        SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
    }

    private const int GWL_STYLE = -16;
    private const long WS_MINIMIZEBOX = 0x00020000, WS_MAXIMIZEBOX = 0x00010000;
    private const uint SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002, SWP_NOZORDER = 0x0004, SWP_FRAMECHANGED = 0x0020;
    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);
    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);

    public void SelectTab(string tag)
    {
        var item = Nav.MenuItems
            .OfType<NavigationViewItem>()
            .FirstOrDefault(item => string.Equals(item.Tag as string, tag, StringComparison.Ordinal));
        if (item != null) Nav.SelectedItem = item;
    }

    private async void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string
            ?? (Nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault()?.Tag as string);
        await ApplyTabAsync(tag);
    }

    private async System.Threading.Tasks.Task ApplyTabAsync(string? tag)
    {
        int loadGeneration = ++_logLoadGeneration;
        ++_logRenderGeneration;
        if (StatusScroll != null) StatusScroll.Visibility = tag == "status" ? Visibility.Visible : Visibility.Collapsed;
        if (UsageTab != null) UsageTab.Visibility = tag == "agents" ? Visibility.Visible : Visibility.Collapsed;
        if (ToolsScroll != null) ToolsScroll.Visibility = tag == "tools" ? Visibility.Visible : Visibility.Collapsed;
        if (CreditsScroll != null) CreditsScroll.Visibility = tag == "credits" ? Visibility.Visible : Visibility.Collapsed;
        if (LogTab != null) LogTab.Visibility = tag == "log" ? Visibility.Visible : Visibility.Collapsed;
        if (tag == "agents") await LoadUsageOnTabSelectedAsync();
        else ++_usageGeneration;
        if (tag == "log") await LoadLogsAsync(loadGeneration); // reload each select
    }

    private int _usageGeneration;
    // ClientSource::CLIENTS order from the skeleton deck.
    private readonly List<string> _usageAgentOrder = new();
    private readonly Dictionary<string, ContentControl> _usageCardBodies = new();
    private readonly Dictionary<string, Border> _usageCardShells = new();
    private readonly Dictionary<string, TextBlock> _usageCardAccounts = new();
    /// <summary>Session-only email reveal (not persisted).</summary>
    private readonly HashSet<string> _usageAccountRevealed = new(StringComparer.Ordinal);
    private readonly Dictionary<string, UsageCardDto> _usageCards = new();
    /// <summary>Last card seen per agent, painted or not — source for a card
    /// materialized by speech (keeps its account label).</summary>
    private readonly Dictionary<string, UsageCardDto> _usageKnown = new();
    /// <summary>Agents heard this launch; each keeps a card even without quota rows.</summary>
    private readonly HashSet<string> _spokenAgents = new(StringComparer.Ordinal);

    private async System.Threading.Tasks.Task LoadUsageOnTabSelectedAsync()
    {
        int generation = ++_usageGeneration;

        UsageDeckDto? deck = null;
        try
        {
            deck = await System.Threading.Tasks.Task.Run(AgentUsageDataSource.ReadCachedDeck);
        }
        catch { /* empty */ }

        if (generation != _usageGeneration || UsageTab.Visibility != Visibility.Visible) return;
        if (deck == null)
        {
            ShowUsageUnavailableIfEmpty();
            return;
        }

        var all = deck.Cards;
        var agentsToLoad = all.Select(c => c.Agent).ToList();
        var cachedWithData = all.Where(c => c.Rows.Count > 0).ToList();
        _usageAgentOrder.Clear();
        _usageAgentOrder.AddRange(agentsToLoad);

        ReconcileUsageAgents(agentsToLoad);
        foreach (var card in all)
            _usageKnown[card.Agent] = card;
        foreach (var card in cachedWithData)
            ApplyUsageCard(card);
        MaterializeSpokenUsageCards();

        if (agentsToLoad.Count == 0)
        {
            ShowUsageUnavailableIfEmpty();
            return;
        }

        var pending = agentsToLoad.Count;
        foreach (string agent in agentsToLoad)
        {
            _ = System.Threading.Tasks.Task.Run(() =>
            {
                UsageCardDto? updated = null;
                try
                {
                    updated = AgentUsageDataSource.RefreshCard(agent);
                }
                catch { /* ignore */ }
                DispatcherQueue.TryEnqueue(() =>
                {
                    if (generation != _usageGeneration) return;
                    if (updated != null)
                    {
                        _usageKnown[updated.Agent] = updated;
                        // Empty results refresh a statless card but never blank a good one.
                        if (updated.Rows.Count > 0 || updated.NeedsAuth || IsUsageStatless(updated.Agent))
                            ApplyUsageCard(updated);
                    }
                    if (--pending == 0)
                        ShowUsageUnavailableIfEmpty();
                });
            });
        }
    }

    private void ReconcileUsageAgents(List<string> installedAgents)
    {
        var installed = installedAgents.ToHashSet(StringComparer.Ordinal);
        foreach (string agent in _usageCardShells.Keys.Where(agent => !installed.Contains(agent)).ToList())
        {
            UsageList.Children.Remove(_usageCardShells[agent]);
            _usageCardShells.Remove(agent);
            _usageCardBodies.Remove(agent);
            _usageCardAccounts.Remove(agent);
            _usageAccountRevealed.Remove(agent);
            _usageCards.Remove(agent);
            _usageKnown.Remove(agent);
        }
    }

    /// <summary>Speech proves the agent is live, so give it a card even when the provider
    /// reports no quota (Qwen Code signed in to z.ai). Painted cards stay painted.</summary>
    private void MaterializeSpokenUsageCards()
    {
        foreach (string agent in _spokenAgents)
        {
            if (!_usageAgentOrder.Contains(agent) || _usageCards.ContainsKey(agent)) continue;
            ApplyUsageCard(_usageKnown.TryGetValue(agent, out var known)
                ? known
                : new UsageCardDto(agent, new List<UsageRowDto>()));
        }
    }

    /// <summary>Painted, but with nothing to show yet.</summary>
    private bool IsUsageStatless(string agent)
        => _usageCards.TryGetValue(agent, out var card) && card.Rows.Count == 0 && !card.NeedsAuth;

    private void ShowUsageUnavailableIfEmpty()
    {
        if (_usageCardBodies.Count > 0) return;
        if (UsageList.Children.Count == 1 && UsageList.Children[0] is TextBlock) return;
        UsageList.Children.Clear();
        UsageList.Children.Add(new TextBlock
        {
            Text = Loc.T("usage.unavailable"),
            Opacity = 0.65,
        });
    }

    private void ApplyUsageCard(UsageCardDto card)
    {
        string agent = card.Agent;
        if (_usageCards.TryGetValue(agent, out var current) && UsageCardsEqual(current, card))
            return;
        _usageCards[agent] = card;

        if (_usageCardBodies.TryGetValue(agent, out var body))
        {
            BindUsageCardHeader(agent, card);
            BindUsageCard(body, card);
            return;
        }

        if (UsageList.Children.Count == 1 && UsageList.Children[0] is TextBlock)
            UsageList.Children.Clear();

        var (shell, newBody) = BuildUsageCardShell(agent);
        BindUsageCardHeader(agent, card);
        BindUsageCard(newBody, card);
        _usageCardBodies[agent] = newBody;
        _usageCardShells[agent] = shell;

        int insertAt = 0;
        foreach (var child in UsageList.Children)
        {
            if (child is not Border b) continue;
            string? existing = null;
            foreach (var kv in _usageCardShells)
            {
                if (ReferenceEquals(kv.Value, b))
                {
                    existing = kv.Key;
                    break;
                }
            }
            if (existing != null && UsageAgentRank(existing) < UsageAgentRank(agent))
                insertAt++;
        }
        UsageList.Children.Insert(insertAt, shell);
    }

    // NeedsAuth in equality so auth transitions repaint.
    private static bool UsageCardsEqual(UsageCardDto left, UsageCardDto right)
        => left.Agent == right.Agent
            && string.Equals(left.Account, right.Account, StringComparison.Ordinal)
            && left.NeedsAuth == right.NeedsAuth
            && left.Rows.SequenceEqual(right.Rows);

    private int UsageAgentRank(string agent)
    {
        int rank = _usageAgentOrder.IndexOf(agent);
        return rank >= 0 ? rank : int.MaxValue;
    }

    /// <summary>Catalog title; unknown tokens → Title Case (not the raw key).</summary>
    private static string UsageProviderTitle(string agent)
    {
        var key = $"usage.provider.{agent}";
        var label = Loc.T(key);
        if (!string.Equals(label, key, StringComparison.Ordinal))
            return label;
        return CultureInfo.InvariantCulture.TextInfo.ToTitleCase(
            agent.Replace('_', ' ').ToLowerInvariant());
    }

    private (Border shell, ContentControl body) BuildUsageCardShell(string agent)
    {
        var body = new ContentControl
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
        };
        var content = new StackPanel
        {
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        // Account opacity 0 until click; session-only (not persisted).
        var heading = new Grid
        {
            ColumnDefinitions = { new ColumnDefinition(), new ColumnDefinition() },
        };
        heading.Children.Add(new TextBlock
        {
            Text = UsageProviderTitle(agent),
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Bottom,
        });
        var account = new TextBlock
        {
            HorizontalAlignment = HorizontalAlignment.Right,
            Opacity = 0,
            FontSize = 12,
            FontFamily = Mono,
            TextAlignment = TextAlignment.Right,
            VerticalAlignment = VerticalAlignment.Bottom,
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = true,
        };
        account.PointerPressed += (_, e) =>
        {
            if (account.Visibility != Visibility.Visible) return;
            if (_usageAccountRevealed.Remove(agent))
                account.Opacity = 0;
            else
            {
                _usageAccountRevealed.Add(agent);
                account.Opacity = 1;
            }
            e.Handled = true;
        };
        Grid.SetColumn(account, 1);
        heading.Children.Add(account);
        _usageCardAccounts[agent] = account;
        content.Children.Add(heading);
        content.Children.Add(body);

        var shell = new Border
        {
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(16),
            BorderThickness = new Thickness(1),
            Child = content,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Background = UsageCardBackground(agent),
        };
        if (Application.Current.Resources.TryGetValue(
                "CardStrokeColorDefaultBrush", out var stroke)
            && stroke is Brush strokeBrush)
            shell.BorderBrush = strokeBrush;
        return (shell, body);
    }

    private void BindUsageCardHeader(string agent, UsageCardDto card)
    {
        if (!_usageCardAccounts.TryGetValue(agent, out var accountLabel)) return;
        string? account = string.IsNullOrWhiteSpace(card.Account) ? null : card.Account.Trim();
        accountLabel.Text = account ?? "";
        if (account == null)
        {
            accountLabel.Visibility = Visibility.Collapsed;
            accountLabel.Opacity = 0;
            _usageAccountRevealed.Remove(agent);
            return;
        }
        accountLabel.Visibility = Visibility.Visible;
        accountLabel.Opacity = _usageAccountRevealed.Contains(agent) ? 1 : 0;
    }

    private void BindUsageCard(ContentControl body, UsageCardDto card)
    {
        if (body.Content is StackPanel mountedRows && TryUpdateUsageRows(mountedRows, card))
            return;

        var rows = new StackPanel
        {
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        foreach (var row in card.Rows)
            rows.Children.Add(BuildUsageRow(row));
        if (card.Rows.Count == 0 && !card.NeedsAuth)
            rows.Children.Add(BuildUsageNoDataRow());
        if (card.NeedsAuth)
            rows.Children.Add(BuildUsageAuthRow(card.Agent));
        body.Content = rows;
    }

    // Same shape → in-place (ProgressBar animates). Auth toggle changes child count → rebuild.
    private static bool TryUpdateUsageRows(StackPanel mountedRows, UsageCardDto card)
    {
        int expected = card.Rows.Count + (card.NeedsAuth ? 1 : 0);
        if (mountedRows.Children.Count != expected) return false;

        for (int i = 0; i < card.Rows.Count; i++)
        {
            if (mountedRows.Children[i] is not StackPanel { Tag: UsageRowView view }
                || view.Period != card.Rows[i].Period)
                return false;
        }
        if (card.NeedsAuth
            && mountedRows.Children[card.Rows.Count] is not Grid { Tag: UsageAuthRowView })
            return false;

        for (int i = 0; i < card.Rows.Count; i++)
        {
            var row = (UsageRowView)((StackPanel)mountedRows.Children[i]).Tag;
            UpdateUsageRow(row, card.Rows[i], animate: true);
        }
        return true;
    }

    private sealed record UsageRowView(
        string Period,
        ProgressBar Progress,
        TextBlock Remaining);

    private sealed record UsageAuthRowView(Button Authorize);

    /// <summary>Placeholder row for a plan the provider publishes no quota for.</summary>
    private static TextBlock BuildUsageNoDataRow()
        => new()
        {
            Text = Loc.T("usage.no_data"),
            Opacity = 0.65,
            HorizontalAlignment = HorizontalAlignment.Left,
        };

    /// <summary>Authorize row — sole UI path that may prompt.</summary>
    private Grid BuildUsageAuthRow(string agent)
    {
        var grid = new Grid
        {
            Margin = new Thickness(0, 3, 0, 3),
            ColumnDefinitions =
            {
                new ColumnDefinition(),
                new ColumnDefinition { Width = GridLength.Auto },
            },
        };
        grid.Children.Add(new TextBlock
        {
            Text = Loc.T("usage.needs_auth"),
            Opacity = 0.65,
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
        });
        var authorize = new Button { Content = Loc.T("usage.authorize") };
        authorize.Click += (_, _) => StartUsageAuthorize(agent, authorize);
        Grid.SetColumn(authorize, 1);
        grid.Children.Add(authorize);
        grid.Tag = new UsageAuthRowView(authorize);
        return grid;
    }

    /// <summary>Blocking authorize FFI off UI; generation-checked apply.</summary>
    private void StartUsageAuthorize(string agent, Button authorize)
    {
        if (!authorize.IsEnabled) return;
        authorize.IsEnabled = false;
        int generation = _usageGeneration;
        _ = System.Threading.Tasks.Task.Run(() =>
        {
            UsageCardDto? updated = null;
            try
            {
                updated = AgentUsageDataSource.AuthorizeCard(agent);
            }
            catch { /* ignore */ }
            DispatcherQueue.TryEnqueue(() =>
            {
                // Re-enable always; deny keeps the same card (no repaint).
                authorize.IsEnabled = true;
                if (generation != _usageGeneration) return;
                if (updated != null && (updated.Rows.Count > 0 || updated.NeedsAuth))
                    ApplyUsageCard(updated);
            });
        });
    }

    private static StackPanel BuildUsageRow(UsageRowDto row)
    {
        var heading = new Grid { ColumnDefinitions = { new ColumnDefinition(), new ColumnDefinition() } };
        heading.Children.Add(new TextBlock
        {
            Text = Loc.T($"usage.{row.Period}"),
            VerticalAlignment = VerticalAlignment.Bottom,
        });
        // Percent is on the bar only; remaining is the reset countdown.
        var remaining = new TextBlock
        {
            HorizontalAlignment = HorizontalAlignment.Right,
            Opacity = 0.65,
            FontSize = 12,
            FontFamily = Mono,
            TextAlignment = TextAlignment.Right,
            VerticalAlignment = VerticalAlignment.Bottom,
        };
        Grid.SetColumn(remaining, 1);
        heading.Children.Add(remaining);

        // Default WinUI track is 1px — bump for readability.
        const double barThickness = 6;
        var progress = new ProgressBar
        {
            Minimum = 0,
            Maximum = 100,
            Height = barThickness,
            MinHeight = barThickness,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Foreground = new SolidColorBrush(Brand.SeedPurple),
        };
        progress.Resources["ProgressBarMinHeight"] = barThickness;
        progress.Resources["ProgressBarTrackHeight"] = barThickness;
        progress.Resources["ProgressBarCornerRadius"] = new CornerRadius(barThickness / 2);
        progress.Resources["ProgressBarTrackCornerRadius"] = new CornerRadius(barThickness / 2);
        var view = new UsageRowView(row.Period, progress, remaining);
        var block = new StackPanel
        {
            Spacing = 5,
            Margin = new Thickness(0, 3, 0, 3),
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Tag = view,
        };
        block.Children.Add(heading);
        block.Children.Add(progress);
        UpdateUsageRow(view, row, animate: false);
        return block;
    }

    private static void UpdateUsageRow(UsageRowView view, UsageRowDto row, bool animate)
    {
        double previous = view.Progress.Value;
        view.Progress.Value = row.UsedPercent;
        if (animate && Math.Abs(previous - row.UsedPercent) >= 0.01)
        {
            var animation = new DoubleAnimation
            {
                From = previous,
                To = row.UsedPercent,
                Duration = new Duration(TimeSpan.FromMilliseconds(180)),
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
                EnableDependentAnimation = true,
            };
            Storyboard.SetTarget(animation, view.Progress);
            Storyboard.SetTargetProperty(animation, nameof(ProgressBar.Value));
            var storyboard = new Storyboard();
            storyboard.Children.Add(animation);
            storyboard.Begin();
        }

        string remaining = Native.UsageResetsIn(row.ResetsAtUnix);
        view.Remaining.Text = remaining;
        view.Remaining.Visibility = string.IsNullOrEmpty(remaining)
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    private List<LogLine> _logLines = new();
    private List<string> _logSources = new();
    private readonly Dictionary<string, SolidColorBrush> _sourceBrush = new();
    private readonly Dictionary<string, SolidColorBrush> _levelBrushCache = new();
    private string _logFilter = "";
    private int _logLoadGeneration;
    private int _logRenderGeneration;

    /// <summary>Load off UI; render in batches. Generation drops stale mid-flight results.</summary>
    private async System.Threading.Tasks.Task LoadLogsAsync(int loadGeneration)
    {
        if (LogText == null) return;
        List<LogLine> lines;
        try
        {
            lines = await System.Threading.Tasks.Task.Run(
                () => LogParser.ParseLogs(Native.LogsJson(64 * 1024)));
        }
        catch { return; }
        if (loadGeneration != _logLoadGeneration || LogTab.Visibility != Visibility.Visible) return;
        _logLines = lines;
        _logSources = LogParser.DistinctSources(_logLines);
        await RenderLogLinesAsync(++_logRenderGeneration);
    }

    private async void LogFilter_TextChanged(object sender, TextChangedEventArgs e)
    {
        _logFilter = LogFilter.Text ?? "";
        await RenderLogLinesAsync(++_logRenderGeneration);
    }

    /// <summary>Confirm then LogsClear. Accent recolored ERROR red; omit DefaultButton so
    /// Enter does not fire Clear past the red override.</summary>
    private async void LogClear_Click(object sender, RoutedEventArgs e)
    {
        var danger = new SolidColorBrush(Brand.LogLevelColor("ERROR") ?? Color.FromArgb(255, 0xE8, 0x46, 0x46));
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = Loc.T("logs.clear_confirm_title"),
            PrimaryButtonText = Loc.T("logs.clear_confirm_action"),
            CloseButtonText = Loc.T("common.cancel"),
            PrimaryButtonStyle = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        dialog.Resources["AccentButtonBackground"] = danger;
        dialog.Resources["AccentButtonBackgroundPointerOver"] = danger;
        dialog.Resources["AccentButtonBackgroundPressed"] = danger;
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            int loadGeneration = ++_logLoadGeneration;
            ++_logRenderGeneration;
            await System.Threading.Tasks.Task.Run(Native.LogsClear);
            if (loadGeneration == _logLoadGeneration && LogTab.Visibility == Visibility.Visible)
                await LoadLogsAsync(loadGeneration);
        }
    }

    /// <summary>Filter + color; yield every 64 so large logs keep input responsive.</summary>
    private async System.Threading.Tasks.Task RenderLogLinesAsync(int renderGeneration)
    {
        LogText.Blocks.Clear();
        var shown = LogParser.Filter(_logLines, _logFilter);
        if (shown.Count == 0)
        {
            var empty = new Paragraph { Margin = new Thickness(0) };
            empty.Inlines.Add(new Run { Text = Loc.T(_logLines.Count == 0 ? "logs.empty" : "logs.no_match"), Foreground = Gray });
            LogText.Blocks.Add(empty);
            return;
        }
        int rendered = 0;
        foreach (var l in shown)
        {
            if (renderGeneration != _logRenderGeneration || LogTab.Visibility != Visibility.Visible) return;
            var para = new Paragraph { Margin = new Thickness(0) };
            para.Inlines.Add(new Run { Text = l.Source, Foreground = SourceBrush(l.Source), FontWeight = FontWeights.SemiBold });
            para.Inlines.Add(new Run { Text = "  " });
            var msgBrush = LevelBrush(l.Level);
            if (l.Level.Length > 0 && l.Level != "INFO")
                para.Inlines.Add(new Run { Text = l.Level + " ", Foreground = msgBrush ?? Gray });
            var msg = new Run { Text = l.Text };
            if (msgBrush != null) msg.Foreground = msgBrush;
            para.Inlines.Add(msg);
            LogText.Blocks.Add(para);
            if (++rendered % 64 == 0) await System.Threading.Tasks.Task.Yield();
        }
        if (renderGeneration != _logRenderGeneration || LogTab.Visibility != Visibility.Visible) return;
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => LogScroll?.ChangeView(null, LogScroll.ScrollableHeight, null, true));
    }

    private SolidColorBrush? LevelBrush(string level)
    {
        if (level.Length == 0) return null;
        if (_levelBrushCache.TryGetValue(level, out var b)) return b;
        if (Brand.LogLevelColor(level) is not Color c) return null;
        var brush = new SolidColorBrush(c);
        _levelBrushCache[level] = brush;
        return brush;
    }

    // First-appearance index into Brand.LogSourcePalette (shared mapping).
    private SolidColorBrush SourceBrush(string source)
    {
        if (_sourceBrush.TryGetValue(source, out var b)) return b;
        var palette = Brand.LogSourcePalette;
        var color = palette.Length == 0
            ? Gray.Color
            : palette[Math.Max(0, _logSources.IndexOf(source)) % palette.Length];
        var brush = new SolidColorBrush(color);
        _sourceBrush[source] = brush;
        return brush;
    }

    private bool _refreshing;

    /// <summary>Bounded off-UI probe (2500ms). Skip while hidden; clear _refreshing even on hang.</summary>
    private async void RefreshStatus()
    {
        if (!AppWindow.IsVisible) return;
        if (_refreshing) return;
        _refreshing = true;
        HealthSnapshot? snap = null;
        try
        {
            var probe = System.Threading.Tasks.Task.Run(HealthSnapshot.Probe);
            var done = await System.Threading.Tasks.Task.WhenAny(
                probe, System.Threading.Tasks.Task.Delay(2500));
            if (done == probe) snap = await probe;
        }
        catch { /* retry next cycle */ }
        finally { _refreshing = false; }

        if (snap is null) return;
        try { ApplyStatus(snap); } catch { /* one bad frame must not kill the loop */ }
    }

    /// <summary>From App's WaitModelStatus thread (already on UI). No-op while hidden.</summary>
    internal void ApplyPushed(HealthSnapshot s)
    {
        // Tray-resident: utterances land while hidden, and the Agents tab needs them later.
        NoteSpokenAgent(s.Activity.Speaker);
        if (!AppWindow.IsVisible) return;
        try { ApplyStatus(s); } catch { /* one bad frame must not kill the push */ }
    }

    /// <summary>Speaker is null unless speaking (Native), so this only latches utterances.</summary>
    private bool NoteSpokenAgent(string? agent)
        => agent is { Length: > 0 } && _spokenAgents.Add(agent);

    private void ApplyStatus(HealthSnapshot s)
    {
        EngineDot.Fill = s.Activity.EngineRunning ? Green : Gray;
        TtsAllTime.Text = Native.DurationLive(s.Lifetime.TtsSecs);
        SttAllTime.Text = Native.DurationLive(s.Lifetime.SttSecs);
        var v = Native.Version();
        VersionText.Text = v.Length > 0 ? v : Loc.T("common.dash");

        // Closed set matches ds-status StatusTtsEngine / StatusSttEngine wire tokens.
        // Unknown → off (never fail-open to built-in labels).
        switch (s.TtsEngine.Engine)
        {
            case "system":
                TtsDetail.Text = Loc.T("status.engine.system");
                ApplyEngine(s.TtsEngine.Status, TtsDot, TtsRing); break;
            case "built_in" when s.TtsEngine.Model is { } model:
                TtsDetail.Text = model switch
                {
                    TtsModel.Chatterbox => Loc.T("status.engine.chatterbox"),
                    TtsModel.Qwen => Loc.T("status.engine.qwen"),
                    TtsModel.OmniVoice => Loc.T("status.engine.omnivoice"),
                    _ => Loc.T("status.engine.kokoro"),
                };
                ApplyEngine(s.TtsEngine.Status, TtsDot, TtsRing); break;
            default: // "off" and anything unexpected
                TtsDetail.Text = "";
                ApplyOff(TtsDot, TtsRing); break;
        }

        switch (s.SttEngine.Engine)
        {
            case "claude_code":
                SttDetail.Text = Loc.T("status.engine.claude_code");
                ApplyEngine(s.SttEngine.Status, SttDot, SttRing); break;
            case "system":
                SttDetail.Text = Loc.T("status.engine.system");
                ApplyEngine(s.SttEngine.Status, SttDot, SttRing); break;
            case "built_in":
                SttDetail.Text = Loc.T("status.engine.parakeet");
                ApplyEngine(s.SttEngine.Status, SttDot, SttRing); break;
            default: // "off" and anything unexpected
                SttDetail.Text = "";
                ApplyOff(SttDot, SttRing); break;
        }

        // Shared formatter returns "" when ready — empty note means show stats (all platforms).
        // Runtime line only when ready (avoids stale "ORT CPU" under Downloading N%).
        bool ttsSystem = s.TtsEngine.Engine == "system";
        var ttsInfo = s.TtsEngine.Status;
        bool ttsTrouble = !string.IsNullOrEmpty(ttsInfo.Word);
        TtsRuntimeRow.Visibility = (!ttsSystem && !ttsTrouble && s.TtsEngine.Provider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (!ttsSystem) TtsRuntimeText.Text = Native.RuntimeLabel(s.TtsEngine.Provider);
        TtsSystemSettingsRow.Visibility = Visibility.Collapsed;
        // Queue sits outside TtsStatsGrid so depth survives every non-stats path — system
        // voice, no_data, and trouble. A download is when utterances actually pile up.
        TtsQueueRow.Visibility = Visibility.Visible;
        TtsQueue.Text = s.Activity.Queued.ToString(System.Globalization.CultureInfo.InvariantCulture);
        if (ttsTrouble)
            ShowMsg(TtsStatsMsg, TtsStatsGrid, ttsInfo.Word);
        else if (ttsSystem)
            ShowSystemVoiceLink();
        else if (s.Tts.Utterances == 0)
            ShowMsg(TtsStatsMsg, TtsStatsGrid, Loc.T("status.no_data"));
        else
        {
            ShowGrid(TtsStatsMsg, TtsStatsGrid);
            TtsSpeed.Text = Native.StatsRange(s.Tts.RtfMin, s.Tts.RtfAvg, s.Tts.RtfMax, 2, "status.stats.unit.times");
            TtsFirst.Text = Native.StatsRange(s.Tts.TtfaMinMs / 1000, s.Tts.TtfaAvgMs / 1000, s.Tts.TtfaMaxMs / 1000, 1, "status.stats.unit.seconds");
            TtsSpoken.Text = Native.StatsCount((ulong)s.Tts.Utterances, s.Tts.AudioSecs);
            TtsFailuresRow.Visibility = s.Tts.Failures > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (s.Tts.Failures > 0)
                TtsFailures.Text = s.Tts.Failures.ToString(System.Globalization.CultureInfo.InvariantCulture);
        }

        bool sttBuiltIn = s.SttEngine.Engine == "built_in";
        var sttInfo = s.SttEngine.Status;
        bool sttTrouble = !string.IsNullOrEmpty(sttInfo.Word);
        SttRuntimeRow.Visibility = (sttBuiltIn && !sttTrouble && s.SttEngine.Provider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (sttBuiltIn) SttRuntimeText.Text = Native.RuntimeLabel(s.SttEngine.Provider);
        if (sttTrouble)
            ShowMsg(SttStatsMsg, SttStatsGrid, sttInfo.Word);
        else if (s.SttEngine.Engine == "claude_code")
            ShowMsg(SttStatsMsg, SttStatsGrid, ClaudeDelegationHint(s));
        else if (s.Stt.Transcriptions == 0)
            ShowMsg(SttStatsMsg, SttStatsGrid, Loc.T("status.no_data"));
        else
        {
            ShowGrid(SttStatsMsg, SttStatsGrid);
            SttSpeed.Text = Native.StatsRange(s.Stt.RtfMin, s.Stt.RtfAvg, s.Stt.RtfMax, 2, "status.stats.unit.times");
            SttTranscribed.Text = Native.StatsCount((ulong)s.Stt.Transcriptions, s.Stt.AudioSecs);
            SttFailuresRow.Visibility = s.Stt.Failures > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (s.Stt.Failures > 0)
                SttFailures.Text = s.Stt.Failures.ToString(System.Globalization.CultureInfo.InvariantCulture);
        }

        if (DiarizationUiEnabled)
        {
            var diarInfo = s.Diarization.Status;
            bool diarTrouble = !string.IsNullOrEmpty(diarInfo.Word);
            ApplyEngine(diarInfo, DiarDot, DiarRing);
            if (diarTrouble)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, diarInfo.Word);
            else if (!s.Diarization.Enabled)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, Loc.T("status.diarization_disabled"));
            else if (s.Diarization.Speakers.Length == 0)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, Loc.T("status.diarization_no_speakers"));
            else
            {
                ShowGrid(DiarStatsMsg, DiarStatsGrid);
                DiarRuntimeRow.Visibility = s.Diarization.Runtime.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
                if (s.Diarization.Runtime.Length > 0) DiarRuntimeText.Text = Native.RuntimeLabel(s.Diarization.Runtime);
                DiarEnrolled.Text = string.Join(", ", s.Diarization.Speakers);
                DiarSensitivity.Text = s.Diarization.ActivityThreshold.ToString("F2", System.Globalization.CultureInfo.InvariantCulture);
            }
        }

        ApplyStateAccent(s.IndicatorState());
        ApplyUsageSpeakingAccent(s.Activity.Speaker);
        CapsDot.Fill = s.Activity.CapsActive ? Green : Gray;
    }

    private TrayIcon.IconState _accentState = (TrayIcon.IconState)(-1);
    private string? _speakingUsageAgent;
    /// Pastel wash; re-rolled when speaking agent changes.
    private Windows.UI.Color? _speakingWash;

    private void SizeStateStripe()
    {
        double h = 48;
        if (FindDescendant(Nav, "TopNavGrid") is FrameworkElement bar && bar.ActualHeight > 0)
            h = bar.ActualHeight;
        StateStripe.Height = h;
    }

    private static FrameworkElement? FindDescendant(DependencyObject root, string name)
    {
        int n = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < n; i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(root, i);
            if (child is FrameworkElement fe && fe.Name == name) return fe;
            if (FindDescendant(child, name) is FrameworkElement hit) return hit;
        }
        return null;
    }

    /// <summary>Top bar wash in tray Brand tints; idle clears. ~30% for Mica readability.</summary>
    private void ApplyStateAccent(TrayIcon.IconState state)
    {
        if (state == _accentState) return;
        _accentState = state;

        var tint = state switch
        {
            TrayIcon.IconState.Recording => Brand.MicOrange,
            TrayIcon.IconState.Speaking => Brand.SeedPurple,
            _ => (Windows.UI.Color?)null,
        };
        if (tint is not Windows.UI.Color basis)
        {
            StateStripe.Background = null;
            return;
        }
        StateStripe.Background = BrandWashBrush(basis);
    }

    /// <summary>Pastel wash on speaking agent card (top bar stays brand purple).</summary>
    private void ApplyUsageSpeakingAccent(string? agent)
    {
        if (NoteSpokenAgent(agent)) MaterializeSpokenUsageCards();
        if (string.Equals(_speakingUsageAgent, agent, StringComparison.Ordinal)) return;
        _speakingUsageAgent = agent;
        _speakingWash = agent is null ? null : Brand.RandomPastelWash();
        foreach (var (name, shell) in _usageCardShells)
            shell.Background = UsageCardBackground(name);
    }

    private Brush UsageCardBackground(string agent)
    {
        if (string.Equals(agent, _speakingUsageAgent, StringComparison.Ordinal))
        {
            var wash = _speakingWash ?? Brand.RandomPastelWash();
            if (wash is Windows.UI.Color c)
            {
                _speakingWash = c;
                return new SolidColorBrush(c);
            }
        }
        if (Application.Current.Resources.TryGetValue(
                "CardBackgroundFillColorDefaultBrush", out var background)
            && background is Brush backgroundBrush)
            return backgroundBrush;
        return new SolidColorBrush(Windows.UI.Color.FromArgb(0, 0, 0, 0));
    }

    private static SolidColorBrush BrandWashBrush(Windows.UI.Color basis)
    {
        const double Tint = 0.30;
        return new SolidColorBrush(
            Windows.UI.Color.FromArgb((byte)(255 * Tint), basis.R, basis.G, basis.B));
    }

    /// <summary>Dot only (trouble note in expansion). Download → orange ring with 0.02 floor
    /// so 0% still shows a sliver (macOS parity).</summary>
    private static void ApplyEngine(EngineInfo e, Microsoft.UI.Xaml.Shapes.Ellipse dot,
                                    Microsoft.UI.Xaml.Controls.ProgressRing ring)
    {
        if (e.State == EngineState.Downloading)
        {
            ring.Value = Math.Clamp(e.Progress, 0.02, 1.0);
            ring.Visibility = Visibility.Visible;
            dot.Visibility = Visibility.Collapsed;
            return;
        }
        ring.Visibility = Visibility.Collapsed;
        dot.Visibility = Visibility.Visible;
        dot.Fill = e.State switch
        {
            EngineState.Running => Green,
            EngineState.Warming => Orange,
            EngineState.Blocked => Orange,
            EngineState.Failed => Red,
            _ => Gray,
        };
    }

    private static void ApplyOff(Microsoft.UI.Xaml.Shapes.Ellipse dot,
                                 Microsoft.UI.Xaml.Controls.ProgressRing ring)
    {
        ring.Visibility = Visibility.Collapsed;
        dot.Visibility = Visibility.Visible;
        dot.Fill = Gray;
    }

    private static string ClaudeDelegationHint(HealthSnapshot s) =>
        s.SttEngine.VoiceKey.Length > 0
            ? Loc.T("status.stt_claude_code", new Dictionary<string, string> { ["key"] = s.SttEngine.VoiceKey })
            : Loc.T("status.stt_claude_code_off");

    private static void ShowMsg(TextBlock msg, FrameworkElement grid, string text)
    {
        msg.Text = text; msg.Visibility = Visibility.Visible; grid.Visibility = Visibility.Collapsed;
    }
    private static void ShowGrid(TextBlock msg, FrameworkElement grid)
    {
        msg.Visibility = Visibility.Collapsed; grid.Visibility = Visibility.Visible;
    }
    private void ShowSystemVoiceLink()
    {
        TtsSystemSettingsText.Text = Loc.T("status.tts_system_settings");
        TtsStatsMsg.Visibility = Visibility.Collapsed;
        TtsStatsGrid.Visibility = Visibility.Collapsed;
        TtsSystemSettingsRow.Visibility = Visibility.Visible;
    }

    private async void VersionLink_Click(object sender, RoutedEventArgs e)
    {
        var url = Native.HomepageUrl();
        if (url.Length > 0 && Uri.TryCreate(url, UriKind.Absolute, out var uri))
            await Windows.System.Launcher.LaunchUriAsync(uri);
    }

    /// <summary>Startup pill in SeedPurple (info, not error/warning). VersionLink still opens homepage.</summary>
    internal void ApplyUpdateCheck(bool available, string? latestVersion)
    {
        if (!available || latestVersion is null) return;
        UpdateBadgeText.Text = latestVersion;
        UpdateArrowText.Visibility = UpdateBadgeText.Visibility = Visibility.Visible;
        var purple = Brand.SeedPurple;
        VersionPill.Background = new SolidColorBrush(Color.FromArgb(40, purple.R, purple.G, purple.B));
    }

    // HyperlinkButton.Click leaves Tapped unhandled — mark it or header expand also fires.
    private void VersionLink_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => e.Handled = true;

    private void TtsSystemSettings_Click(object sender, RoutedEventArgs e) => Native.OpenVoiceSettings();

    private void DontSpeakHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(DontSpeakStats);
    private void TtsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(TtsStats);
    private void SttHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(SttStats);
    private void DiarHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(DiarStats);
    private void CapsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(CapsStats);
    private static void ToggleStats(FrameworkElement panel)
    {
        panel.Visibility = panel.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
    }

    private const double StatusChromeDip = 84;

    // Last auto-fit client height (-1 = never). Match ⇒ still tracking; else honor user taller size.
    private int _lastFitClientPx = -1;

    /// <summary>Min height = Status content; auto-fit unless user dragged taller.
    /// Use arranged ActualHeight — manual Measure blanks the window.</summary>
    private void CapHeightToStatusContent()
    {
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter pr) return;
        if (StatusScroll?.Content is not FrameworkElement panel || Content?.XamlRoot is null) return;
        double scale = Content.XamlRoot.RasterizationScale;
        if (scale <= 0 || panel.ActualHeight <= 0) return;
        int clientPx = (int)Math.Ceiling((panel.ActualHeight + StatusChromeDip) * scale);
        int nonClientPx = Math.Max(0, AppWindow.Size.Height - AppWindow.ClientSize.Height);
        pr.PreferredMinimumHeight = clientPx + nonClientPx;
        pr.PreferredMaximumHeight = null;
        // Height-only resize skips panel SizeChanged — safe from feedback loops.
        int cur = AppWindow.ClientSize.Height;
        bool atAutoFit = _lastFitClientPx < 0 || Math.Abs(cur - _lastFitClientPx) <= 2;
        if (atAutoFit || cur < clientPx)
        {
            if (Math.Abs(cur - clientPx) > 2)
                AppWindow.ResizeClient(new Windows.Graphics.SizeInt32(AppWindow.ClientSize.Width, clientPx));
            _lastFitClientPx = clientPx;
        }
    }

    private void LoadTools()
    {
        string json = Native.ToolsJson();
        if (string.IsNullOrWhiteSpace(json)) return;
        List<ToolDto>? tools;
        try { tools = JsonSerializer.Deserialize<List<ToolDto>>(json, ToolsJsonOptions); }
        catch { return; }
        if (tools is null) return;

        foreach (var tool in tools)
        {
            var name = tool.Name ?? "";
            if (name.Length == 0) continue;

            var body = new StackPanel { Spacing = 10 };
            var desc = tool.Description ?? "";
            if (desc.Length > 0)
                body.Children.Add(new TextBlock { Text = desc, TextWrapping = TextWrapping.Wrap, Opacity = 0.75 });

            var ps = tool.Params ?? new List<ToolParamDto>();
            if (ps.Count == 0)
            {
                body.Children.Add(new TextBlock { Text = Loc.T("tools.no_arguments"), FontSize = 12, Opacity = 0.5 });
            }
            else
            {
                body.Children.Add(new TextBlock
                {
                    Text = Loc.T("tools.arguments").ToUpperInvariant(),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Opacity = 0.5,
                    CharacterSpacing = 60,
                });
                foreach (var p in ps)
                {
                    var pname = p.Name ?? "";
                    if (pname.Length == 0) continue;

                    var head = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
                    head.Children.Add(new TextBlock { Text = pname, FontFamily = Mono, FontSize = 13, FontWeight = FontWeights.Medium, VerticalAlignment = VerticalAlignment.Center });
                    head.Children.Add(new TextBlock { Text = string.IsNullOrEmpty(p.Type) ? Loc.T("tools.param.type_any") : p.Type, FontSize = 12, Opacity = 0.6, VerticalAlignment = VerticalAlignment.Center });
                    var req = new TextBlock { Text = p.Required ? Loc.T("tools.param.required") : Loc.T("tools.param.optional"), FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
                    if (p.Required) req.Foreground = Orange; else req.Opacity = 0.6;
                    head.Children.Add(req);
                    var detail = p.Detail ?? "";
                    if (detail.Length > 0)
                        head.Children.Add(new TextBlock { Text = detail, FontSize = 12, Opacity = 0.6, VerticalAlignment = VerticalAlignment.Center });

                    var prow = new StackPanel { Spacing = 1 };
                    prow.Children.Add(head);
                    var pdesc = p.Description ?? "";
                    if (pdesc.Length > 0)
                        prow.Children.Add(new TextBlock { Text = pdesc, FontSize = 12, Opacity = 0.55, TextWrapping = TextWrapping.Wrap });
                    body.Children.Add(prow);
                }
            }

            ToolsList.Children.Add(new Expander
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Header = new TextBlock { Text = name, FontFamily = Mono, FontWeight = FontWeights.SemiBold },
                Content = body,
            });
        }
    }

    private static readonly JsonSerializerOptions ToolsJsonOptions = new() { PropertyNameCaseInsensitive = true };

    private void LoadLibraries()
    {
        string json = Native.LibrariesJson();
        if (string.IsNullOrWhiteSpace(json)) return;
        List<LibraryDto>? projects;
        try { projects = JsonSerializer.Deserialize<List<LibraryDto>>(json, ToolsJsonOptions); }
        catch { return; }
        if (projects is null) return;

        foreach (var p in projects)
        {
            var name = p.Name ?? "";
            if (name.Length == 0) continue;

            var body = new StackPanel { Spacing = 10 };

            var usage = p.Usage ?? "";
            if (usage.Length > 0)
                body.Children.Add(new TextBlock { Text = usage, TextWrapping = TextWrapping.Wrap, Opacity = 0.75 });

            var languages = p.Languages ?? new List<string>();
            if (languages.Count > 0 || p.LanguageCount is not null)
            {
                body.Children.Add(new TextBlock
                {
                    Text = Loc.T("libraries.languages").ToUpperInvariant(),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Opacity = 0.5,
                    CharacterSpacing = 60,
                });
                var languageSummary = p.AutomaticLanguageDetection && p.LanguageCount is long count
                    ? Loc.T("libraries.automatic_languages", new Dictionary<string, string>
                    {
                        ["count"] = count.ToString("N0", CultureInfo.CurrentCulture),
                    })
                    : string.Join(", ", languages
                        .Where(code => !string.Equals(code, "auto", StringComparison.Ordinal))
                        .Select(code => Loc.T($"language.{code}")));
                body.Children.Add(new TextBlock
                {
                    Text = languageSummary,
                    TextWrapping = TextWrapping.Wrap,
                    Opacity = 0.75,
                });
                if (!string.IsNullOrEmpty(p.LanguageListUrl) && Uri.TryCreate(p.LanguageListUrl, UriKind.Absolute, out var languageList))
                    body.Children.Add(new HyperlinkButton { Content = Loc.T("libraries.full_language_list"), NavigateUri = languageList, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            }

            var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
            if (!string.IsNullOrEmpty(p.Homepage) && Uri.TryCreate(p.Homepage, UriKind.Absolute, out var hp))
                links.Children.Add(new HyperlinkButton { Content = Loc.T("libraries.homepage"), NavigateUri = hp, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            var lic = p.License ?? "";
            if (lic.Length > 0 && !string.IsNullOrEmpty(p.LicenseUrl) && Uri.TryCreate(p.LicenseUrl, UriKind.Absolute, out var lu))
                links.Children.Add(new HyperlinkButton { Content = lic, NavigateUri = lu, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            if (links.Children.Count > 0) body.Children.Add(links);

            var files = p.Files ?? new List<LicenseFileDto>();
            if (files.Count > 0)
            {
                body.Children.Add(new TextBlock
                {
                    Text = Loc.T("libraries.files").ToUpperInvariant(),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Opacity = 0.5,
                    CharacterSpacing = 60,
                });
                foreach (var f in files)
                {
                    var fname = f.Name ?? "";
                    if (fname.Length == 0) continue;
                    var row = new Grid
                    {
                        ColumnDefinitions =
                        {
                            new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) },
                            new ColumnDefinition { Width = GridLength.Auto },
                        },
                    };
                    row.Children.Add(new TextBlock { Text = fname, FontFamily = Mono, FontSize = 12, TextWrapping = TextWrapping.Wrap, Opacity = 0.8 });
                    if (f.SizeBytes is long sz && sz > 0)
                    {
                        var sizeTb = new TextBlock { Text = Native.HumanSize((ulong)sz), FontSize = 12, Opacity = 0.5, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 0, 0) };
                        Grid.SetColumn(sizeTb, 1);
                        row.Children.Add(sizeTb);
                    }
                    body.Children.Add(row);
                }
            }

            var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
            header.Children.Add(new TextBlock { Text = name, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });

            CreditsList.Children.Add(new Expander
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Header = header,
                Content = body,
            });
        }
    }

    // Wire: ds-model libraries::catalog.
    private sealed record LibraryDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("usage")] string? Usage,
        [property: JsonPropertyName("homepage")] string? Homepage,
        [property: JsonPropertyName("license")] string? License,
        [property: JsonPropertyName("license_url")] string? LicenseUrl,
        [property: JsonPropertyName("languages")] List<string>? Languages,
        [property: JsonPropertyName("language_count")] long? LanguageCount,
        [property: JsonPropertyName("automatic_language_detection")] bool AutomaticLanguageDetection,
        [property: JsonPropertyName("language_list_url")] string? LanguageListUrl,
        [property: JsonPropertyName("files")] List<LicenseFileDto>? Files);

    private sealed record LicenseFileDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("url")] string? Url,
        [property: JsonPropertyName("size_bytes")] long? SizeBytes);

    // Wire: ds-tools catalog_ui.
    private sealed record ToolDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("description")] string? Description,
        [property: JsonPropertyName("params")] List<ToolParamDto>? Params);

    private sealed record ToolParamDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("type")] string? Type,
        [property: JsonPropertyName("required")] bool Required,
        [property: JsonPropertyName("description")] string? Description,
        // Pre-built by status_fmt::tool_param_detail — host paints only.
        [property: JsonPropertyName("detail")] string? Detail);
}
