# Harvesting the WinUI publish tree

The WinUI app (`Atlas.App.exe`) is published **self-contained**
(`SelfContained` + `WindowsAppSDKSelfContained` in `Atlas.App.csproj`), so the
publish folder is not one file - it is a few hundred: `Atlas.App.exe`, the
Windows App SDK runtime, the .NET runtime, WinUI native DLLs, `resources.pri`,
localized satellite assemblies, etc. Hand-authoring a `<Component>`/`<File>` for
each is unmaintainable and drifts every time the SDK version bumps.

## Approach used: the WiX v5 `<Files>` element (auto-harvest)

`Package.wxs` harvests the whole tree at build time:

```xml
<ComponentGroup Id="cgAppPayload" Directory="INSTALLFOLDER">
  <Files Include="$(var.AppPublishDir)\**">
    <Exclude Files="$(var.AppPublishDir)\**\*.pdb" />
    <Exclude Files="$(var.AppPublishDir)\**\*.xml" />
  </Files>
</ComponentGroup>
```

- `<Files>` is the modern, in-language replacement for the old `heat.exe`
  harvester (heat is no longer shipped in the WiX v4/v5 .NET tool). No separate
  harvest step, no generated `.wxs` to check in.
- Each discovered file becomes its own component with a **stable GUID derived
  from its install path**, so the component set tracks the publish output
  automatically and upgrades replace files correctly (one file per component is
  the MSI best practice for clean patching).
- Subdirectories under the publish root are recreated under
  `INSTALLFOLDER` automatically by the `**` recursion.
- `Atlas.App.exe` is harvested here too. The Start Menu shortcut targets it by
  path (`[INSTALLFOLDER]Atlas.App.exe`), **not** by generated File Id, so the
  shortcut never depends on a harvest-assigned id.

Verified against a placeholder publish tree: nested files are packaged and the
`.pdb`/`.xml` excludes are honored (the compiled MSI's `File` table contained
`Atlas.App.exe`, the runtime DLLs, and `resources.pri`, but no `.pdb`).

## Why not a hand-authored ComponentGroup

A manual `ComponentGroup` (one `<Component>`+`<File>` per payload file) is the
fallback if you ever need per-file control (e.g. marking a specific DLL as the
keypath, custom permissions on one file, or companion-file relationships). It is
rejected as the default here purely on maintenance cost: the WindowsAppSDK
payload changes with every SDK bump and would require regenerating the list each
time. If you do need it, generate a starting point once with:

```
wix build ... -o app.wixpdb   # then read the File/Component tables
```

and paste the components into a dedicated `AppFiles.wxs`, but prefer `<Files>`.

## Determinism / reproducibility note

`<Files>`-generated component GUIDs are a deterministic function of the install
path, so two builds of the same publish layout produce the same component ids -
important for clean major upgrades. If a future publish reorganizes folder
layout, component ids for moved files change; MajorUpgrade (scheduled
`afterInstallInitialize`) handles that by removing the old product first.
