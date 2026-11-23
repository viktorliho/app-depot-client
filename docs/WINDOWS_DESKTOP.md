# App Depot Desktop for Windows 11 x64

## Install and connect

Download `AppDepot-Desktop-Windows-x64-Setup.exe` and `SHA256SUMS` from the same approved release. Compare the setup hash with its entry in the manifest using `Get-FileHash -Algorithm SHA256 .\AppDepot-Desktop-Windows-x64-Setup.exe`. This verifies file integrity. The unsigned candidate can show an unknown publisher or SmartScreen warning. Use only the expected release source and follow your Windows security policy.

Run Setup as your normal user. The default program folder is `%LOCALAPPDATA%\Programs\App Depot Desktop`. Setup adds a Start menu entry, registers the app in Installed Apps and offers an optional desktop shortcut. Rust, Git and Visual Studio are build-time dependencies. The app uses the Windows D3D12 stack and needs a working D3D12-compatible graphics driver.

Open **App Depot Desktop** from Start. Add your server URL under **Servers**, connect as Guest when available, and sync the catalog. Managed Windows-native releases require a compatible NativePortableV1 launch declaration. Open the release details to Download, Install, Verify, Play or Uninstall the working copy.

Exit Desktop before updating or removing Desktop from Windows Settings. An active managed launch may keep the supervisor executable open, so let its game and descendants exit when an update needs to replace the application binary. Installing an update over the existing location leaves the library alone. Removing Desktop through **Settings → Apps → Installed apps** removes application binaries and shortcuts. Profiles, verified downloads, installed games and saves stay in the library.

## Release candidate notes

- Windows File and Tree downloads, offline Verify, native Play and managed process supervision use the shared client. Managed Uninstall keeps the completed download and separate saves, so Install can reuse verified content.
- Uninstall refuses running or uncertain managed processes, modified or unknown content, sharing conflicts, reparse points, unexpected hard links and named NTFS streams. The recovery journal retains incomplete operations. The private managed library is assumed to be under the user's control, without a hostile same-user process modifying it concurrently. External or escaped processes are outside universal detection.
- Windows Repair, physical CAS garbage collection, automatic Desktop updates and Windows 10 are deferred for this release. Windows Server 2022 previously reported sharing violation 32 during File/Tree tests. See [the milestone diagnostics](MILESTONES.md#m41b2b--windows-removal-preflight).

## Build and package the candidate

From an x64 Visual Studio 2022 developer prompt with Windows SDK, Rust 1.100.0 and Inno Setup 6.7:

```powershell
powershell -NoProfile -ExecutionPolicy RemoteSigned -File scripts\build-windows.ps1 -OutputDirectory C:\build\app-depot-windows -Candidate 1
```

Pass `-InnoCompiler` if ISCC is elsewhere. The script compiles the optimized locked MSVC target with a static CRT, embeds the bundled icon and Windows version resource, checks PE architecture, subsystem and runtime imports, and checksums the installer and Desktop executable. The supervisor uses a mode of that same executable. For an executable-only build, use `-BinaryOnly`.

Client CI builds the same installer and exercises per-user setup, reinstall/update, registration, binary integrity and standard uninstallation on a disposable Windows Server 2025 runner. Download the **AppDepot-Desktop-Windows-x64-RC** workflow artifact for evaluation. This environment does not substitute for native Windows 11 GUI acceptance.

The first public Windows candidate should use a distinct prerelease tag such as `windows-v0.1.3-rc.1` on the exact verified main commit, marked prerelease and explicitly excluded from Latest. Attach only setup and the corresponding checksum manifest, plus the standalone executable if desired. Publish after owner approval and Windows 11 acceptance. The Linux AppImage release workflow uses `v*` tags and the existing stable release remains separate.

## Clean Windows 11 acceptance

Use a fresh Windows 11 x64 account or VM without development tools and with a supported graphics driver. Check the setup hash, install without elevation, launch via Start, add the server as Guest, verify covers and release details, restart and verify the saved profile and catalog. Disconnect temporarily and verify the UI reports the error and can reconnect.

Use a separate library and a disposable Windows-native Dev/Test release to download, install, verify and launch both File and Tree. Test Uninstall while the game is running and after exit, then reinstall and launch again. Restart Desktop with a managed child running and confirm removal remains refused. Repeat Setup over the existing installation, then uninstall Desktop through Settings and check that those library records and saves survive.
