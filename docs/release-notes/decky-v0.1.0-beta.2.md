App Depot Decky beta.2 adds first-run LAN discovery and Guest access. Download **AppDepot-Decky-Installer.desktop**, permit execution in SteamOS Desktop Mode and double-click it. An existing Decky Loader installation is required. The same installer upgrades beta.1 while preserving profiles, credentials, shortcut bindings and local content.

In Gaming Mode, open Decky → App Depot. Select a discovered Server or enter its address manually. Test Guest access and use a token only where required. For HTTP, acknowledge the unencrypted connection before testing or saving. Guest sees only Public Published content after the Server administrator enables anonymous reading.

An administrator can opt in to advertising the Server via DNS-SD. Configure that independently of guest access. [Discovery deployment and Steam Deck acceptance](https://github.com/viktorliho/app-depot-client/blob/decky-v0.1.0-beta.2/docs/STEAMOS_DECKY.md)

This prerelease targets SteamOS x86-64. Discovery on the owner's network and Gaming Mode behavior still need device validation.
