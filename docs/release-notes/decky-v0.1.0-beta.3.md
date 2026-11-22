App Depot Decky beta.3 corrects shortcut creation observed on SteamOS 3.9.5. It reads back the actual Steam AppID before setting the name, executable, working directory and launch options. An exact beta.2 malformed shortcut can be repaired in place when the App Depot binding and Steam records confirm ownership. Artwork starts after Steam acknowledges correct shortcut metadata.

Upgrade with **AppDepot-Decky-Installer.desktop** in Desktop Mode. Return to Gaming Mode, open Decky → App Depot and synchronize the existing profile. App Depot keeps the same owned shortcut AppIDs. If Play exits before a window appears, the launcher records a redacted stage and exit code in the user state directory.

The two affected shortcuts were corrected and read back on a Steam Deck. A disposable native fixture downloaded, launched and ran again offline using the device's Gaming Mode display. Real game launch through the Steam Play button remains part of hardware acceptance.
