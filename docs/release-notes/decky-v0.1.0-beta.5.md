## App Depot for Steam Deck

- **Installed** uses compact application rows and a separate **Uninstall** action. The confirmation explains which local data can be lost. While removal runs, the row shows an indeterminate indicator and elapsed time.
- Shortcut reconciliation, artwork retries and foreground actions report their results separately. Uninstall still retains the Steam shortcut and uses the same local recovery and shared-content safeguards.
- **Updates & About** shows the actual Decky version. GitHub checks explicitly use the system CA bundle on SteamOS, retain HTTPS verification and distinguish rate limits, connectivity and invalid release metadata.

Install `AppDepot-Decky-Installer.desktop` in Desktop Mode. In Gaming Mode, check the reported version and **Check for updates**, then use a disposable application to verify Uninstall and the next Play. [Steam Deck acceptance steps](https://github.com/viktorliho/app-depot-client/blob/main/docs/STEAMOS_DECKY.md#steam-deck-hardware-acceptance)
