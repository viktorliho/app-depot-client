## SteamOS Desktop Mode installer fix

- The single-file `AppDepot-Decky-Installer.desktop` download uses a compact launcher to avoid the oversized command that Dolphin could rewrite into a Python prompt.
- The launcher verifies the versioned installer script before running it. The installer still supports resumable ZIP downloads, pinned SHA-256 verification and rollback after a failed Decky restart.
- Install or update in Desktop Mode with Decky Loader already installed. In Dolphin, enable execution under Properties → Permissions, double-click the file and confirm installation. Check the version in App Depot's Settings & About after returning to Gaming Mode.
