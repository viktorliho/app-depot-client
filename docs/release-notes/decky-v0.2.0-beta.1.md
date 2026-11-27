# App Depot Decky v0.2.0-beta.1

Play an uninstalled native Linux or declared Windows application from its existing Steam shortcut. The gamepad-first window shows the installation plan, prerequisite consent and measured transfer progress. It distinguishes file preparation, prefix initialization, prerequisite execution and launch. Use the D-pad or stick to navigate, A to confirm and B to go back or cancel. Resume or retry from the wrapper.

After installation the game starts automatically. Subsequent Play reuses the installation. Uninstall from Decky's Installed section to return the existing shortcut to grayscale and enable First Play again. Removal checks ownership and running processes and preserves shared content still in use. The plugin shows the application name and elapsed time during removal.

For Windows applications select an already installed compatibility runtime in Decky settings. Prerequisites and the game share the application prefix. The owner accepted Overlord II installation and gameplay on a physical Steam Deck with Steam Proton Experimental. Native Linux and Windows fixtures also completed uninstall, reinstall and offline replay through Gaming Mode. Third-party installers can require interaction. If Steam assigns an unavailable game display, Decky gives instructions for restarting the Gaming Mode session.

## Installation and upgrade

With Decky Loader installed, download `AppDepot-Decky-Installer.desktop` from this release. In Desktop Mode enable execution in Dolphin, open the installer and confirm the local sudo prompt if requested. The installer verifies the versioned script and ZIP before activation. Failures in the installation script are recorded in `~/AppDepot-Decky-Installer.log`. Return to Gaming Mode and check version 0.2.0-beta.1 under Decky → App Depot → Settings & About.
