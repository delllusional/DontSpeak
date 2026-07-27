using System;
using System.Threading.Tasks;

namespace DontSpeak;

/// <summary>Runs tray-triggered native calls away from the UI thread.</summary>
internal static class TrayCommand
{
    public static Task<bool> RunAsync(Func<bool> command) => Task.Run(command);
}
