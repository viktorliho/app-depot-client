# App Depot Desktop for Windows x64

The owner accepted Windows Desktop v0.1.4 as part of the first unified stable Linux and Windows release. The Windows MVP is complete and in maintenance mode. Deferred Windows work includes Repair, physical CAS garbage collection, extended Windows 10 lifecycle validation, Windows Server 2022 compatibility investigation and automatic Desktop update rollback. These are outside the accepted MVP scope.

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

Historical Windows-only prereleases may retain their tags. Stable Windows packages now ship with Linux under one annotated Desktop `vX.Y.Z` tag. The release workflow passes `-Stable` to the build script so the installer and installed executable report the shared product version. For explicitly assigned Desktop release tasks, the workflow publishes only after both platforms and every required draft asset pass verification. Routine main commits run CI and produce candidates. See [Distribution](DISTRIBUTION.md).

## Desktop release channels

- Windows x64 checks the official repository from **Updates & About**. It selects only complete stable Desktop `vX.Y.Z` releases with both current platforms. It skips drafts, prereleases, former Windows-only tags and Decky releases. RCs remain manual downloads.
- A stable Windows release supplies `AppDepot-Desktop-Windows-x64-Setup.exe` and `SHA256SUMS`. The updater validates the exact GitHub URLs and requires the checksum manifest and GitHub metadata to agree. It checks the installer size, SHA-256 and PE header during staging and again before launching Setup. Cancel or discard before handoff removes the temporary download.
- Updates & About asks for confirmation before starting Setup and closing Desktop. Managed game activity or uncertain occupancy defers handoff. Setup also checks the Desktop and supervisor mutex and verifies the registered per-user installation directory. A successful Setup relaunches Desktop. If Setup is cancelled or fails, open Desktop from Start and retry from Updates & About. Setup owns the installer after handoff, and its staged file may remain in the temporary directory. Automatic rollback is unavailable.
- Linux uses the same complete-release discovery and keeps the existing AppImage validation, handoff and rollback path. If Latest is incomplete or belongs to a separate channel, both clients search bounded release history for a complete stable Desktop version. macOS joins the same release contract once supported.

For update acceptance on a disposable Windows 11 account, use the Windows CI artifact `AppDepot-Windows-Update-Fixtures`. Install A, set `APP_DEPOT_WINDOWS_UPDATE_FIXTURE` to the extracted B directory and set `APP_DEPOT_LIBRARY_DATA_DIR` and `APP_DEPOT_LIBRARY_CONTENT_DIR` to disposable directories before starting the installed Desktop. Check for updates in Updates & About and complete Setup. Verify About, the server profile and the managed library after relaunch. Both packages use the production handoff. Only fixture builds can read local release metadata, and official builds reject the fixture feature.
- Inno Setup uses an application mutex held by Desktop and its supervisor processes in the interactive Windows session. Setup and the Desktop uninstaller wait for those processes to exit or allow the user to cancel. They do not close games or supervisors automatically. Close Desktop and let managed games exit before starting Setup.

## Short Windows 10 hardware acceptance

On the personal PC, check the setup hash, install normally, launch from Start, connect to TrueNAS as Guest and confirm the catalog renders. Close and reopen Desktop to confirm the profile persists. Optionally remove Desktop through Windows Settings. Preserve library data and other applications. This checks basic compatibility. Use the Windows 11 development VM or disposable CI systems for File/Tree, Play, managed Uninstall, recovery and stress tests.

## Clean Windows 11 acceptance

Use a fresh Windows 11 x64 account or VM without development tools and with a supported graphics driver. Check the setup hash, install without elevation, launch via Start, add the server as Guest, verify covers and release details, restart and verify the saved profile and catalog. Disconnect temporarily and verify the UI reports the error and can reconnect.

Use a separate library and a disposable Windows-native Dev/Test release to download, install, verify and launch both File and Tree. Test Uninstall while the game is running and after exit, then reinstall and launch again. Restart Desktop with a managed child running and confirm removal remains refused. Repeat Setup over the existing installation, then uninstall Desktop through Settings and check that those library records and saves survive.
