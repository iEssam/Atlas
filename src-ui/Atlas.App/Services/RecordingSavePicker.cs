using System.ComponentModel;
using System.Runtime.InteropServices;
using Atlas.IpcClient;

namespace Atlas.App.Services;

internal sealed record RecordingSaveTarget(
    string Path,
    string FileName,
    GamingRecordingFormat Format);

/// <summary>
/// Uses the desktop common-file dialog because brokered WinRT pickers fail when
/// this unpackaged diagnostics app is elevated for ETW capture.
/// </summary>
internal static class RecordingSavePicker
{
    private const int OfnNoChangeDir = 0x00000008;
    private const int OfnPathMustExist = 0x00000800;
    private const int OfnExplorer = 0x00080000;
    private const int FileBufferLength = 32_768;

    private const string Filter =
        "JSON recording (*.json)\0*.json\0CSV recording (*.csv)\0*.csv\0\0";

    public static RecordingSaveTarget? Pick(nint ownerWindow, string suggestedFileName)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(suggestedFileName);

        var filterBuffer = Marshal.StringToHGlobalUni(Filter);
        var fileBuffer = Marshal.AllocHGlobal(FileBufferLength * sizeof(char));
        var titleBuffer = Marshal.StringToHGlobalUni("Export gaming recording");
        var extensionBuffer = Marshal.StringToHGlobalUni("json");
        try
        {
            var initialFileName = $"{suggestedFileName}.json";
            Marshal.Copy(initialFileName.ToCharArray(), 0, fileBuffer, initialFileName.Length);
            Marshal.WriteInt16(fileBuffer, initialFileName.Length * sizeof(char), 0);

            var dialog = new OpenFileName
            {
                StructSize = Marshal.SizeOf<OpenFileName>(),
                OwnerWindow = ownerWindow,
                Filter = filterBuffer,
                FilterIndex = 1,
                File = fileBuffer,
                MaxFile = FileBufferLength,
                Title = titleBuffer,
                Flags = OfnExplorer | OfnNoChangeDir | OfnPathMustExist,
                DefaultExtension = extensionBuffer,
            };

            if (!GetSaveFileName(ref dialog))
            {
                var error = CommDlgExtendedError();
                if (error == 0)
                {
                    return null;
                }

                throw new Win32Exception(unchecked((int)error), $"The Windows Save dialog failed (0x{error:X4}).");
            }

            var format = dialog.FilterIndex == 2
                ? GamingRecordingFormat.Csv
                : GamingRecordingFormat.Json;
            var extension = format == GamingRecordingFormat.Csv ? ".csv" : ".json";
            var selectedPath = Marshal.PtrToStringUni(fileBuffer)
                ?? throw new InvalidOperationException("The Windows Save dialog returned an empty path.");
            var path = Path.ChangeExtension(selectedPath, extension);
            return new RecordingSaveTarget(path, Path.GetFileName(path), format);
        }
        finally
        {
            Marshal.FreeHGlobal(extensionBuffer);
            Marshal.FreeHGlobal(titleBuffer);
            Marshal.FreeHGlobal(fileBuffer);
            Marshal.FreeHGlobal(filterBuffer);
        }
    }

    [DllImport("comdlg32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetSaveFileNameW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetSaveFileName(ref OpenFileName dialog);

    [DllImport("comdlg32.dll")]
    private static extern uint CommDlgExtendedError();

    [StructLayout(LayoutKind.Sequential)]
    private struct OpenFileName
    {
        public int StructSize;
        public nint OwnerWindow;
        public nint Instance;
        public nint Filter;
        public nint CustomFilter;
        public int MaxCustomFilter;
        public int FilterIndex;
        public nint File;
        public int MaxFile;
        public nint FileTitle;
        public int MaxFileTitle;
        public nint InitialDirectory;
        public nint Title;
        public int Flags;
        public short FileOffset;
        public short FileExtension;
        public nint DefaultExtension;
        public nint CustomData;
        public nint Hook;
        public nint TemplateName;
        public nint Reserved;
        public int ReservedSize;
        public int ExtendedFlags;
    }
}
