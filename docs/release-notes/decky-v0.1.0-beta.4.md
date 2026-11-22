## App Depot for Steam Deck

This prerelease is for hardware acceptance of local Uninstall. It requires Decky Loader and updates an existing App Depot Decky installation.

- **Installed** shows each local application's Server profile, readiness, logical storage and active-operation status. Confirm **Uninstall** to remove App Depot-managed local content for that application and profile. Saves, settings and modifications inside managed folders may be permanently deleted. The operation works offline. External data, shared content still in use and the Steam shortcut remain.
- The existing shortcut keeps its AppID and Steam Input settings. Managed grid artwork returns to grayscale when its source is available. The next Play uses the existing First Play flow and can reuse shared verified content.
- **Updates & About** checks the Decky beta channel, displays release notes and verifies downloads. Use Desktop Mode to install an update.
- Guest access, LAN discovery and existing First Play behavior carry over from beta.3.

Run `AppDepot-Decky-Installer.desktop` in Desktop Mode, then return to Gaming Mode. For the first uninstall test, use a disposable app whose managed saves and modifications can be deleted. [Steam Deck acceptance steps](https://github.com/viktorliho/app-depot-client/blob/fac14c50cb8812dd02badcff2686f3cac42ea0ea/docs/STEAMOS_DECKY.md#steam-deck-hardware-acceptance)
