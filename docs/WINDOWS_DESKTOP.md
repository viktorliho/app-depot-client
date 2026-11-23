# App Depot Desktop for Windows x64

## Install and connect

Setup accepts Windows 10 version 2004 (build 19041) or later and Windows 11 on x64. The owner confirmed RC2 installs and starts from the Start menu on a physical Windows 10 PC, displays the Guest catalog and artwork and preserves its profile across restart. File/Tree operations, Play and recovery on Windows 10 still require separate validation on an isolated test system.

Download `AppDepot-Desktop-Windows-x64-Setup.exe` and `SHA256SUMS` from the same approved release. Compare the setup hash with its entry in the manifest using `Get-FileHash -Algorithm SHA256 .\AppDepot-Desktop-Windows-x64-Setup.exe`. This verifies file integrity. The unsigned candidate can show an unknown publisher or SmartScreen warning. Use only the expected release source and follow your Windows security policy.

Run Setup as your normal user. The default program folder is `%LOCALAPPDATA%\Programs\App Depot Desktop`. Setup adds a Start menu entry, registers the app in Installed Apps and offers an optional desktop shortcut. Rust, Git and Visual Studio are build-time dependencies. The app uses the Windows D3D12 stack and needs a working D3D12-compatible graphics driver.

Open **App Depot Desktop** from Start. Add your server URL under **Servers**, connect as Guest when available, and sync the catalog. Managed Windows-native releases require a compatible NativePortableV1 launch declaration. Open the release details to Download, Install, Verify, Play or Uninstall the working copy.

Exit Desktop before updating or removing Desktop from Windows Settings. An active managed launch may keep the supervisor executable open, so let its game and descendants exit when an update needs to replace the application binary. Installing an update over the existing location leaves the library alone. Removing Desktop through **Settings → Apps → Installed apps** removes application binaries and shortcuts. Profiles, verified downloads, installed games and saves stay in the library.

## Release candidate notes

- Windows File and Tree downloads, offline Verify, native Play and managed process supervision use the shared client. Managed Uninstall keeps the completed download and separate saves, so Install can reuse verified content.
- Uninstall refuses running or uncertain managed processes, modified or unknown content, sharing conflicts, reparse points, unexpected hard links and named NTFS streams. The recovery journal retains incomplete operations. The private managed library is assumed to be under the user's control, without a hostile same-user process modifying it concurrently. External or escaped processes are outside universal detection.
- Windows Repair and physical CAS garbage collection are deferred.
- Windows Server 2022 (build 20348) previously reported sharing violation 32 during File/Tree publication. Those paths request delete access for handle-relative, no-replace rename. Another handle without delete sharing can block that operation on Windows 10 too. The original CI logs lack the first failing API call, so reproduction on Windows 10 is still unknown. Preserve the sharing constraints and use the existing first-error traces and focused File/Tree tests on a disposable Windows 10 NTFS VM if the issue reproduces. [Milestone diagnostics](MILESTONES.md#m41b2b--windows-removal-preflight) · [Microsoft file sharing documentation](https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-createfilew)

## Build and package the candidate

From an x64 Visual Studio 2022 developer prompt with Windows SDK, Rust 1.100.0 and Inno Setup 6.7:

```powershell
powershell -NoProfile -ExecutionPolicy RemoteSigned -File scripts\build-windows.ps1 -OutputDirectory C:\build\app-depot-windows -Candidate 2
```

Pass `-InnoCompiler` if ISCC is elsewhere. The script compiles the optimized locked MSVC target with a static CRT, embeds the bundled icon and Windows version resource, checks PE architecture, subsystem and runtime imports, and checksums the installer and Desktop executable. The supervisor uses a mode of that same executable. For an executable-only build, use `-BinaryOnly`.

Client CI checks the version floor and x64 configuration in the actual Inno Setup file. It then builds the installer and exercises per-user setup, reinstall/update, registration, binary integrity and standard uninstallation on a disposable Windows Server 2025 runner. Download the **AppDepot-Desktop-Windows-x64-RC** workflow artifact for evaluation.

When a dedicated disposable Windows 10 x64 workstation runner is available, give it the `app-depot-windows-10` label and manually dispatch **Client CI** on `main` with `windows_10_smoke` enabled. That job consumes the exact setup artifact produced by the workflow, verifies the OS and exercises the same install, reinstall and uninstall smoke checks. It needs no build tools on the test machine. Keep personal PCs off the runner pool.

Historical Windows-only prereleases may retain their tags. Stable Windows packages now ship with Linux under one annotated Desktop `vX.Y.Z` tag. The release workflow passes `-Stable` to the build script so the installer and installed executable report the shared product version. The workflow requires both platforms before making the release public.

## Desktop release channels

- Windows x64 checks the official repository from **Updates & About**. It selects only complete stable Desktop `vX.Y.Z` releases with both current platforms. It skips drafts, prereleases, former Windows-only tags and Decky releases. RCs remain manual downloads.
- A stable Windows release supplies `AppDepot-Desktop-Windows-x64-Setup.exe` and `SHA256SUMS`. The updater validates the exact GitHub URLs, checks the manifest against GitHub's asset size and SHA-256, and requires the manifest's installer SHA-256 to agree with GitHub's. The installer download is bounded by the exact expected size, checked against the agreed digest and validated as a PE executable before it is exposed in a temporary folder. Cancel or discard removes the temporary download. To install at this checkpoint, copy the verified installer outside that folder before closing Desktop and run Setup manually. Automatic handoff and relaunch are pending.
- Linux uses the same complete-release discovery and keeps the existing AppImage validation, handoff and rollback path. If Latest is incomplete or belongs to a separate channel, both clients search bounded release history for a complete stable Desktop version. macOS joins the same release contract once supported.

## Short Windows 10 hardware acceptance

On the personal PC, check the setup hash, install normally, launch from Start, connect to TrueNAS as Guest and confirm the catalog renders. Close and reopen Desktop to confirm the profile persists. Optionally remove Desktop through Windows Settings. Preserve library data and other applications. This checks basic compatibility. Use the Windows 11 development VM or disposable CI systems for File/Tree, Play, managed Uninstall, recovery and stress tests.

## Clean Windows 11 acceptance

Use a fresh Windows 11 x64 account or VM without development tools and with a supported graphics driver. Check the setup hash, install without elevation, launch via Start, add the server as Guest, verify covers and release details, restart and verify the saved profile and catalog. Disconnect temporarily and verify the UI reports the error and can reconnect.

Use a separate library and a disposable Windows-native Dev/Test release to download, install, verify and launch both File and Tree. Test Uninstall while the game is running and after exit, then reinstall and launch again. Restart Desktop with a managed child running and confirm removal remains refused. Repeat Setup over the existing installation, then uninstall Desktop through Settings and check that those library records and saves survive.
