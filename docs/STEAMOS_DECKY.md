# SteamOS Decky beta

M3.1a packages an unprivileged Decky backend, a Decky control surface and an x86-64 Linux Rust launcher. The launcher is independent of the Desktop AppImage. Steam supplies the catalog and Play UI. The Server, Installed and Settings panels cover connection, synchronization, local readiness and diagnostics.

## Build and test on Debian

Use Debian 13 x86-64, Rust 1.95.0 with rustfmt and clippy, and Node 26.5.0 with npm 11.17.0. Install build-essential, pkg-config, libssl-dev and libdbus-1-dev as for Desktop, plus libudev-dev for gamepad support. Python 3 and python3-vdf are needed for the Decky tests. python3-pil is only needed for the M3.0 fixture suite. The compiled plugin needs no Node, Rust, compiler or Python packages on the Deck.

~~~sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo clippy -p app-depot --features decky --all-targets --locked -- -D warnings
cargo test -p app-depot --features decky --locked
cargo build --release -p app-depot --features decky --locked
npm --prefix decky ci
npm --prefix decky test
npm --prefix decky run build
/usr/bin/python3 -B -m unittest discover -s decky -v
/usr/bin/python3 -B -m unittest discover -s tools/steamos -v
~~~

The launcher is a Linux x86-64 ELF. The Debian build references glibc through 2.39 and needs the GNU loader, libgcc, libdbus and libudev. X11/Wayland, EGL/GL and input libraries are also loaded at runtime. Check the target Deck with `getconf GNU_LIBC_VERSION` and `ldd ~/homebrew/plugins/AppDepot/bin/app-depot` if launch fails. [Valve's SteamOS package mirror](https://steamdeck-packages.steamos.cloud/archlinux-mirror/holo-3.9/os/x86_64/)

To run the real Server fixture with the packaged build on Debian, build Server with its pinned Rust 1.88.0. For the X11 extension install xvfb, xauth, libxkbcommon-x11-0, a software Mesa GL stack, dbus-x11 and gnome-keyring. Use a disposable session bus and keyring. The following command runs the M3.0 HTTP suite as well as the Decky backend setup, authenticated sync, artwork, GUI download, reload and display-free offline Play.

~~~sh
(cd ../app-depot-server && cargo build -p app-depot-server --locked)
fixture=$(mktemp -d)
mkdir -m 700 "$fixture/runtime"
export XDG_DATA_HOME="$fixture/data" XDG_RUNTIME_DIR="$fixture/runtime"
xvfb-run -a dbus-run-session -- sh -ec '
  printf fixture-keyring-password | gnome-keyring-daemon --unlock --components=secrets
  exec /usr/bin/python3 -B tools/steamos/verify_first_play.py     --server "$PWD/../app-depot-server/target/debug/app-depot-server"     --client "$PWD/target/release/app-depot"     --client-repo "$PWD" --decky-plugin "$PWD/decky/main.py"
'
~~~

## Package and installer

~~~sh
out="$HOME/app-depot-dev/releases/m3.1a.1"
/usr/bin/python3 -B decky/package.py   --binary target/release/app-depot --out "$out" --development-loopback
(cd "$out" && sha256sum -c SHA256SUMS)
desktop-file-validate "$out/AppDepot-Decky-Installer.desktop"
~~~

The ZIP contains AppDepot/plugin.json, package.json, main.py, dist/index.js, bin/app-depot and LICENSE. Packaging uses fixed entry times, ordering and modes. The installer embeds the exact versioned URL and SHA-256. It validates both before unpacking, rejects foreign paths, non-regular archive entries and unexpected identities, and revalidates the hash in the privileged activation step. Installation locks the Decky plugin directory and stages on the same filesystem. Updates atomically exchange the two directories and roll back on a failed service restart. The backend runs as the Steam user. A healthy Decky service restart does not prove frontend activation. [Decky installer service and directory conventions](https://github.com/SteamDeckHomebrew/decky-installer/tree/e42a3c828de90673959b4de26cf5c5d2c7830554)

For the public beta, download only [AppDepot-Decky-Installer.desktop](https://github.com/viktorliho/app-depot-client/releases/download/decky-v0.1.0-beta.3/AppDepot-Decky-Installer.desktop) from the [GitHub prerelease](https://github.com/viktorliho/app-depot-client/releases/tag/decky-v0.1.0-beta.3). In Desktop Mode, permit execution in KDE, double-click the installer and confirm. Enter the Desktop Mode password if sudo prompts. After installation, return to Gaming Mode and open **Decky → App Depot**.

The dedicated Decky workflow builds public beta tags using `decky/package.py --github-release-tag TAG`. It publishes a GitHub prerelease and checks that the Desktop latest release still has the same ID. The generated installer pins the release ZIP URL and checksum. Its package downloader accepts one HTTPS redirect from the official GitHub release URL to GitHub's release-asset host. App Depot Server requests use the Rust core's separate redirect policy and reject redirects. The development-loopback build remains available for disposable installation tests.

App Depot credentials use the existing native Secret Service adapter via the Steam user's session D-Bus. If a token cannot be saved because the service or unlocked collection is missing, configuration reports the failure. There is no plaintext token fallback. Use HTTPS when available. Any valid HTTP origin is supported after enabling **Allow unencrypted HTTP** during setup. HTTP sends Bearer tokens without transport encryption, even though they remain in Secret Service at rest. As on Desktop, a saved HTTP profile retains its chosen origin for sync, artwork and First Play. Loopback HTTP also works without an opt-in. HTTPS uses normal TLS verification. The client uses the configured scheme and rejects Server redirects. Stable launcher aliases and the integration ledger persist in `~/.local/share/app-depot-decky`. App content remains in the shared library, usually `~/.appdepot`. Preserve both on upgrades.

Steam mutations use the internal SteamClient API. A saved pending record is written before creation. The returned numeric AppID is checked by reading Steam's live app details, comparing the stored target and launch arguments. After an interrupted create without a returned ID, the backend may recover from a unique match in the saved Steam shortcut snapshot. Pending creation stays pending when proof is missing. This prevents automatic duplicates after an ambiguous failure. Resolve the unresolved item in a disposable acceptance environment before considering manual binding repair. The backend reads `shortcuts.vdf` for recovery and icon ownership and never writes it. Artwork uses Steam's API for grid, hero and logo. The icon uses a managed per-shortcut PNG and SteamClient's icon setter. A matching saved image digest confirms a grid update on the next sync. A differing user image or icon is preserved. When a role disappears from the Server, the MVP leaves the previous Steam image in place. Installation-state observation runs every 30 seconds while Decky is loaded and attempts color restoration from the Server.

## Guest access and LAN discovery

In Gaming Mode the first-run Server panel searches for three seconds. Select a candidate or enter an address manually. Candidates remain unverified until **Test connection** or **Save and synchronize** checks the HTTP API. The default is Guest. A successful empty catalog is valid and only includes Public Published Apps. If the Server requires authentication, the panel explains the requirement and opens the token option. A guest probe never uses a saved token. An existing authenticated profile keeps its token, profile ID and shortcut bindings. For HTTP, explicitly acknowledge that credentials travel without encryption.

An App Depot administrator can enable guest access in the Server Portal under **Admin → Server Settings → Anonymous read access → Enable anonymous read → Save setting**. That setting does not publish an App or its Releases. The guest catalog requires Public visibility and publication, including a Published Release for downloadable content. Private and Draft content still requires appropriate credentials. Users who should retain access to Private content can continue using a token. Discovery and guest access are independent.

### Discovery advertisement

App Depot uses DNS-SD service type `_appdepot._tcp.local.`, over UDP 5353 multicast on the local link. The DNS-SD instance name is the friendly name. The SRV record names the host and published API port. Use only these required TXT keys:

| Key | Value |
| --- | --- |
| `v` | `1` |
| `scheme` | `http` or `https` |

Publish the actual client-reachable endpoint. Do not put tokens, App lists, account names or filesystem details in records. The client uses a resolved advertised address for HTTP, preferring IPv4, and preserves the SRV hostname for HTTPS so normal certificate validation applies. The HTTPS name must resolve from the client and match a trusted certificate. Use manual entry if the advertised name or address is unsuitable. IPv6 addresses that require an interface zone are omitted. Each search is a fresh, bounded snapshot and removes goodbye records received during that search. DNS-SD results can be spoofed. Select a known origin before entering a token.

The shared Rust API is `app_depot_core::discovery::discover(Duration)`, which returns `ServerCandidate { name, origin }` values. The same API is available to future Desktop clients. It requires no privileged discovery daemon on SteamOS and performs no network scan or HTTP probes while browsing.

The existing App Depot Server can be advertised by a separate DNS-SD publisher without changing its listen address, authorization policy or API. On a Linux LAN host with Avahi, install a service file in Avahi's configured service directory, commonly `/etc/avahi/services/app-depot.service`. A service published on the API host can omit `host-name`. Use the reachable published port.

~~~xml
<?xml version="1.0" standalone="no"?>
<!DOCTYPE service-group SYSTEM "avahi-service.dtd">
<service-group>
  <name>Home App Depot</name>
  <service>
    <type>_appdepot._tcp</type>
    <port>8787</port>
    <txt-record>v=1</txt-record>
    <txt-record>scheme=http</txt-record>
  </service>
</service-group>
~~~

For Docker/Compose or TrueNAS SCALE, keep the App Depot container on its existing network. Advertise the published host port or the reachable reverse-proxy port. An administrator can use an existing LAN-side Avahi publisher, or run a dedicated publisher on a host-networked container subject to platform support and operator policy. If the publisher is on another LAN machine, set `<host-name>` to the API endpoint's existing resolvable mDNS name and publish that endpoint's port. For TrueNAS prefer a managed publisher or a separate LAN machine to persistent manual edits of appliance system files. Do not point the advertisement at a container-only DNS name or private bridge address.

Check the chosen interface with `avahi-browse -rt _appdepot._tcp` from a second LAN machine. A separate publisher must share the Deck's multicast link, or the network must explicitly support forwarding the relevant mDNS service between segments. Guest Wi-Fi, VLAN boundaries and multicast filtering can block discovery. Confirm UDP 5353 activity and, if an existing mDNS daemon owns the port, publish through that daemon to avoid binding conflicts. If discovery stays empty, use the manual address. [Avahi service files](https://github.com/avahi/avahi/blob/master/man/avahi.service.5.xml.in) · [DNS-SD RFC](https://www.rfc-editor.org/rfc/rfc6763)

## Shortcut setup and launcher diagnostics

Steam on SteamOS 3.9.5 derived the initial name from the executable and left launch options unset on the two observed beta.2 creations. It also stored a second pair of quotes around the quoted executable it received. The installed Steam UI calls AddShortcut with the name, executable, arguments and full command line. The adapter now passes those fields in that order, with an unquoted executable and a quoted executable within the command line, saves the returned AppID and explicitly sets missing shortcut fields after readback. A legacy entry is eligible only when the saved AppID, expected launcher alias and exact malformed live and disk fields agree. Setup is resumable and stops if unrelated metadata changes. Artwork starts after correct metadata and ownership are confirmed. Steam writes its own VDF.

Inspect the latest native launch record at `$XDG_STATE_HOME/app-depot-decky/last-launch.json` or, when the environment variable is unset, `~/.local/state/app-depot-decky/last-launch.json`. The record contains stage, PID and exit code. It contains no credentials, command line or Server name. The record is written when the launcher starts. An unchanged record after Play is evidence to inspect Steam's executable and launch options before diagnosing transfer or game failures.

## M3.1b hardware acceptance

1. Record SteamOS, Steam client and Decky versions. Start with Decky installed. In Desktop Mode permit execution of `AppDepot-Decky-Installer.desktop`, launch it, confirm the operation and verify the restart succeeds. Return to Gaming Mode and check that App Depot opens in Decky. Repeat installation to check idempotence. Keep the Deck online until the pinned GitHub package finishes downloading.
2. Verify beta.1 profiles and tokens survived the upgrade. Open **Discover Servers** and choose the known advertisement. If discovery fails, enter the original Server address manually. Try Guest, or enable **Use an access token** if Guest is prohibited. For HTTP, enable **Allow unencrypted HTTP**. Test, then save and synchronize. Guest only lists Public Published items. Confirm Unreal Gold and Unreal Tournament 1999 through a profile authorized to see them. Do not change their visibility for this test.
3. Record actual shortcut IDs and set a custom name, artwork and Steam Input layout on a disposable App. Sync twice, synchronize two equally named Apps or two profiles, and rename an App at the Server. Check persisted IDs, user settings, artwork and absence of duplicate/foreign changes. Restart Steam, reload Decky and repeat. Try an upgrade while preserving library and bindings. Verify whether icon and artwork changes need a restart, including logos without a prior custom position.
4. Choose an undownloaded native App. Use the Deck controls for any choice and observe transfer phases. Cancel during download, then Retry. Confirm checkpoint resume and launch. Stop an uninstalled Play while its Server is offline and confirm Retry/Exit. Restore connectivity and Retry. Start an installed App offline and check it opens without a download window.
5. Check the native game's sustained focus, built-in controls, Steam Controller if available, Steam Input layouts, overlay, Stop Game, normal exit, descendant processes, playtime and return to Steam. Confirm no transfer process remains after cancellation and no game remains after Stop Game. Check that the parent waits while the game runs.
6. Check Steam artwork in portrait and wide slots before and after download, verify transparency and other roles, then change Server artwork and sync. Check persistence after Steam restart and preservation of a user override. Confirm a missing art role never prevents shortcut creation.
7. Start with internal storage. With owner approval test an existing library split-root configuration on a microSD fixture, including card removal and return, before offering a location switch in Decky. The MVP displays the selected shared library location. Alternate-root UI is deferred. Windows Apps remain visible and the MVP does not configure Proton. Test actual SteamOS binary loading and Secret Service availability in Gaming Mode separately.

Debian verification covers the real Server, native X11 under Xvfb, Secret Service, an offline launch without a display, child exit code and SIGTERM propagation, mocked SteamClient and VDF readback, and package extraction/activation rollback with a fixture service. The hardware cases above are still open.

## Updates & About

The M3.2 candidate can check the official Decky beta channel from Gaming Mode. Checks are user initiated. The panel shows the installed Decky package version, the highest applicable Decky version, release notes and the official versioned release page. Diagnostics identifies the shared core version separately. **Download and verify** checks the ZIP in temporary storage, then discards it. To activate an update, download and run that release's `AppDepot-Decky-Installer.desktop` in Desktop Mode after exiting any game. It downloads its own pinned copy and restarts Decky.

The release picker compares numeric `decky-vMAJOR.MINOR.PATCH-beta.N` versions over a bounded paginated listing from the official GitHub repository. It ignores Desktop releases, drafts and older versions. The candidate must be a prerelease with one of each expected asset at the exact versioned URL. The ZIP SHA-256 comes from GitHub's authenticated HTTPS Release API `digest` field. This model trusts GitHub and the repository's release publishers. An unavailable digest or inconsistent asset blocks verification. The API forbids redirects. Decky uses the system CA bundle explicitly when available, because Loader's bundled OpenSSL can have unusable default trust paths on SteamOS. The checker uses a short in-memory page cache and ETag revalidation. TLS, rate limits, connectivity and malformed metadata have separate messages. The release ZIP accepts only the GitHub release-assets HTTPS handoff and is checked against its digest, exact file inventory, plugin identity, package version and launcher ELF header. [GitHub release asset API](https://docs.github.com/en/rest/releases/assets)

On the tested SteamOS 3.9.5 installation with Decky Loader 3.3.1, both the plugin directory and installed App Depot were root-owned with mode 0755. The App Depot backend runs as the Deck user. Loader's privileged install operation uninstalls the previous version before checking the ZIP hash and extracts directly into the plugin directory. Its import path has no transactional rollback or plugin health acknowledgement. The existing Desktop installer stages and exchanges directories on the same filesystem, holds an installation lock and rolls back when the activation callback fails. A successful service restart does not establish plugin health. [Decky Loader 3.3.1 install implementation](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/browser.py#L245-L301)

Gaming Mode activation requires a supported transactional Loader API or a separately authorized, narrowly scoped activation component. A live test must cover queueing while a game or transfer is active, capability detection, atomic replacement, bounded plugin health acknowledgement, recovery after interruption and rollback. Once a supported mechanism exists, install the first updater-enabled version using its official Desktop installer. With the Deck idle, check for the subsequent authorized prerelease in Gaming Mode, inspect notes, verify the download and explicitly activate it. Check that the new version opens in Decky and that an installed native game opens offline with its saved shortcut identity, artwork and controls. Repeat with an intentionally failing disposable candidate and confirm the previous plugin returns intact. Beta.3 has no update controls.

### Proposed activation boundary

Prefer a supported Decky transaction with equivalent guarantees if one becomes available. For Loader versions without it, the candidate design is an opt-in, socket-activated system service restricted to the explicitly enrolled Deck user. The Desktop installer would explain the added authority and install the root-owned executable, units, signing public key and enrollment together after explicit administrator approval. The socket must live below a root-owned directory with access restricted to the enrolled account. The helper must verify the caller UID via kernel peer credentials. Authenticated UID bounds which account may request installation. It does not distinguish the Decky plugin from another process running as that user, so the protocol must remain safe for any process with that UID. Restrict the service to the socket, staging, transaction state and App Depot destination. It needs neither direct Internet access nor access to the game library. [systemd socket activation](https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html) · [systemd service sandboxing](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)

- **Authority.** Expose status and one activation request for App Depot. Accept only a bounded, signed release manifest and ZIP bytes via IPC. Pin the repository, plugin identity, exact version/channel, package digest, size and archive layout in the signed contract. Require a release signature under a production key pinned at enrollment, monotonic eligible versions, and a separately reviewed key rotation procedure. GitHub HTTPS and asset digests used by the preview do not authorize privileged activation. No caller-supplied destination, executable, service name, script, arguments or environment reaches a privileged operation.
- **Staging and trust.** Read the upload into exclusive root-owned storage, verify the signed manifest and digest there, and extract those same bytes through directory-relative operations. Refuse links, traversal, special files, unexpected entries and excessive size. Open and validate root-owned directories without symlink traversal and hold pinned directory handles across verification and activation. Loader must resolve the same trusted destination on reload. A root-owned leaf under user-replaceable ancestors is insufficient for this guarantee. Confirm a supported protected path or reject activation on that layout. Extract plugin files as inert data and load the plugin only under its existing unprivileged Decky identity. The release schema must reject privilege-escalating plugin metadata.
- **Coordination.** Serialize helper transactions and hold an exclusive lease spanning replacement and validation. Introduce a shared lease acquired by App Depot launch/transfer entry points before they enter the affected lifecycle and held through game exit or transfer completion. Defer activation while the lease is busy and block new launch startup while the exclusive lease is held. Test this with installed launchers that predate the lease, and require a validated bootstrap transition before activation can be enabled. Avoid a whole-Steam restart. Reload App Depot through a validated Loader operation or require a separately confirmed idle Decky restart if no isolated reload exists.
- **Recovery.** Persist a root-owned transaction journal and fsync the package directories and state before and after atomic exchange. Retain a previous generation until a bounded, generation-specific health check confirms the expected version is serving normal RPC through Loader. Treat this check as a liveness signal, never as an integrity check. On timeout, failed reload or interrupted exchange, identify the actual on-disk generation, restore the previous one, reload it and report the outcome. Recovery must handle a crash at each durable transition. The previous generation is eligible only for recovery initiated by the in-flight transaction.
- **Removal.** Offer a separately confirmed Desktop Mode removal flow. Wait for or recover any active transaction, disable and remove only the updater's units, enrollment, executable and updater-owned staging, then reload systemd. Leave the Decky plugin, user library, bindings and credentials in place.

Before any implementation is installed on a Deck, a disposable Linux harness must exercise peer authorization, package tampering, symlinks and traversal, concurrent requests, a running game holding the lease, requests arriving during activation, health timeout and crashes before and after exchange. A Loader-specific harness and a separately approved Deck test must then demonstrate that the enrollment, intended reload operation and rollback work on the supported runtime. No privileged helper is shipped by this milestone.

## Steam library installation filters

Read-only Steam inspection on SteamOS 3.9.5 found the two downloaded Unreal games in core state `Ready` and as non-Steam shortcuts with both `AppOverview.installed` and `local_per_client_data.installed` set to true. That sample does not establish what Steam reports for a shortcut whose App Depot content is absent. The running `SteamClient.Apps` surface exposed shortcut creation and metadata setters and no shortcut installation-state setter. The installed Steam UI bundle also excludes shortcuts explicitly from some native local-game and ready-to-play collections in Gamepad UI. Private in-memory overview updates would not establish persistent native installation state or reliable Play behavior.

All SteamOS installation-tab integration is deferred. App Depot keeps its non-Steam shortcuts and current artwork behavior, with grayscale grids before installation and color after installation. Native Steam installation state, default tabs and custom Steam library UI remain under Steam's control. App Depot does not depend on TabMaster.

### Deferred alternatives from the TabMaster investigation

TabMaster was inspected at commit `b58f503dfe18abdf67d7766998ad77259f2d20fa` (3.1.0, GPL-3.0 and BSD-3-Clause). The Deck has TabMaster 2.15.1. Its installed source map confirms use of the private `/library` patch. An offline experiment verified that native membership could be joined with independently verified App Depot readiness without duplicates or changes to native Steam data. No integration approach was selected. [TabMaster source and license](https://github.com/Tormak9970/TabMaster/tree/b58f503dfe18abdf67d7766998ad77259f2d20fa)

- **A. Compose TabMaster filters.** TabMaster can combine predicates, use an AppID whitelist and match Steam collections. Its installed predicate uses Steam's `AppOverview.installed`. A union would need a native-only branch and a dynamic App Depot whitelist. No documented external provider or tab registration API was found. The internal manager persists visibility, ordering and filters.
- **B. Replace the views independently.** TabMaster patches the Library route and nested React components, intercepts tab construction and uses a Steam grid template. A second replacement could hide existing tabs or interfere with unpatching in either order. Coexistence and reload ordering were not validated.
- **C. Add separate App Depot tabs.** An insertion through the same private route patch would share the coexistence and reload risks. No stable, composable insertion interface was established.

## M3.3 local application uninstall

In **App Depot → Installed**, select **Uninstall** for the chosen App and Server profile, then confirm or cancel. The list is local and covers non-archived Server profiles. It displays readiness, active-operation blocks and logical usage of managed writable files and verified downloads. Filesystem sharing and compression can make physical reclaim lower. Partial download staging and CAS reclaim are not included in the displayed estimate.

Confirmation authorizes complete local removal for that App and profile across cached Releases and Associations. This includes writable runtimes, managed user data, verified download references, download records and staging selected by those Associations. Local saves, settings and modifications within the managed directories can be permanently deleted. Unrelated external files, other profiles or Apps, profiles and credentials, Server data and Steam settings remain outside the removal scope. Shared CAS objects remain while referenced. If CAS cleanup cannot prove safe reclamation, the UI reports that some unused storage could not be reclaimed.

The core revalidates the selection revision and checks running games, download activity and pending runtime preparation under its lifecycle and download locks. Each selected Association is durably marked before deletion begins. Interrupted removal resumes on library open. Cancel is available while confirming. Once removal starts, the Installed row shows an indeterminate indicator and elapsed time while the same core operation runs. The result remains attached to the selected application. Diagnostics reports background synchronization and deferred artwork separately. Removal uses local library data and requires no Server connection.

Uninstall never reconciles Steam shortcut creation or metadata. It records grid artwork work in the binding ledger before starting local removal. Only the existing owned portrait and landscape slots are refreshed. An acknowledged color PNG with a matching hash can be transformed offline. If a required source is absent and the Server cannot be reached, the transition remains pending and is retried. User-owned artwork overrides and other roles are preserved. The next Play follows normal First Play and may reuse verified shared CAS bytes. A later Play launches the prepared runtime directly.

### Steam Deck hardware acceptance

Use an owner-approved disposable App and the authorized release candidate.

1. Synchronize, record the actual Steam AppID, artwork and Steam Input layout. Use Play to install and confirm a direct second Play.
2. While the game runs, check **Installed** reports an active operation. Exit the game. Open **Uninstall**, read the local-data deletion warning, then cancel and verify Play still works.
3. Disconnect the network. Confirm **Uninstall** for the disposable App. Check that it leaves **Installed** and its shortcut, AppID, launch options and Steam Input settings remain. Verify cached grid artwork becomes grayscale. Missing artwork can remain pending until the Server is reachable.
4. Reconnect and use the same shortcut to launch. Verify the normal First Play flow and restoration of color artwork. Shared cache hits may avoid a network transfer. Relaunch offline and confirm the prepared runtime starts directly.
