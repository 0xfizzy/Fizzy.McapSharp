# Release configuration

The build workflow builds win-x64 native code, runs managed tests, checks both directions against the official Python MCAP implementation for all three compression modes, and produces the 0.1.0 NuGet package. Native assets are required for packing. Tests use temporary files and require no devices.

The publish workflow is manual and uses `NuGet/login@v1` with GitHub OIDC Trusted Publishing. Configure the repository's NuGet trusted publisher, the `nuget` GitHub environment and `NUGET_USER` secret before invoking it. The workflow rebuilds and tests before pushing. No credentials are embedded and no publish is performed by local build scripts.

Consumers normally use PackageReference. A source integration may conditionally select ProjectReference with `UseFizzyMcapSharpSource` and `FizzyMcapSharpRoot`; never select both forms for the same assembly. Build the native runtime before restoring/building a source consumer. The managed project propagates the native DLL to source consumers' outputs. After changing source/package mode, restore again.

Local isolated tools under `.tools` are ignored; Build.ps1 uses them when present, otherwise the installed Cargo. Toolchain installations must not be committed. Build artifacts live under `native/target`, bin/obj and artifacts and are ignored.
