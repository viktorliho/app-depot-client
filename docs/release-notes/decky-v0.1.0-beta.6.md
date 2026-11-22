## App Depot Decky installer reliability

The single-file Desktop installer displays the download size, speed and current phase. It retries interrupted transfers with bounded backoff and resumes from verified range metadata when the CDN provides a strong ETag. A healthy download can continue beyond the connection and idle timeout limits.

The installer still requires the embedded SHA-256 to match the complete package and checks the archive before replacing the plugin. Existing rollback and Decky compatibility are retained.

Download and run the new `AppDepot-Decky-Installer.desktop` in SteamOS Desktop Mode. It pins this release's ZIP and can upgrade an existing App Depot installation.
