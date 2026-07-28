# Explorer integration

System Atlas exposes **Find what is using this file** for one selected filesystem file. The native `IExplorerCommand` is a thin forwarder: it validates the selection and starts `Atlas.App.exe --find-using <absolute-path>`. The app opens the existing File Locks page and performs the Restart Manager lookup through the service.

## Build

```powershell
./shell-ext/build.ps1 -Configuration Release -Platform x64 -Version 0.3.0.0
```

This produces `SystemAtlas.ShellExtension.dll` and an unsigned sparse identity `.msix` under `shell-ext/out`. The DLL architecture must match Explorer. The package is architecture-neutral because its content lives at the external install location.

## Sign and register for development

Sign the `.msix` with a certificate whose subject matches its manifest publisher (`CN=System Atlas Project` by default), trust that certificate, then place the DLL beside the published `Atlas.App.exe` and run:

```powershell
./shell-ext/register.ps1 -PackagePath <signed-msix> -ExternalLocation <published-app-directory>
```

Restart File Explorer from Task Manager, or sign out and back in. Remove the registration with `./shell-ext/unregister.ps1`.

The WiX MSI also registers a classic-menu fallback. On Windows 11 it appears under **Show more options** even when the sparse package is not registered; a signed sparse package is required for the primary modern menu.

Production releases must sign the native DLL, sparse package, app, and MSI. The build does not create or trust a certificate and does not restart Explorer.

Design and deployment follow Microsoft’s [File Explorer context-menu command guidance](https://learn.microsoft.com/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer) and [external-location package identity guidance](https://learn.microsoft.com/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps).
