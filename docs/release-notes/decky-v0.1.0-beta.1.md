App Depot Decky beta makes Steam Library the App Depot catalog. It requires SteamOS on x86-64 and an existing Decky Loader installation. This is a hardware-validation beta.

1. Download **AppDepot-Decky-Installer.desktop** from this release.
2. In SteamOS Desktop Mode, permit execution in KDE and double-click it.
3. Confirm the install and supply your Desktop Mode password if sudo prompts. The installer verifies Decky Loader, downloads the versioned ZIP and checks its embedded SHA-256 before activation.
4. Return to Gaming Mode and open **Decky → App Depot**. Enter the Server URL and token, test the connection, then select **Save and synchronize**.

HTTPS and HTTP Server URLs are supported. For HTTP, enable **Allow unencrypted HTTP**. Bearer tokens on that connection are transmitted without transport encryption and are stored through the user's Secret Service. No server redirect is followed.

Live Steam shortcut APIs, artwork refresh, and controller/session behavior still require validation on Steam Deck. Windows Apps are visible. Windows execution through Proton is outside this beta.
