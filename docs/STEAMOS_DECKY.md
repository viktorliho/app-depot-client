# SteamOS Decky beta

M3.1a packages an unprivileged Decky backend, a Decky control surface and an x86-64 Linux Rust launcher. The launcher is independent of the Desktop AppImage. Steam supplies the catalog and Play UI. The Server, Installed and Settings panels cover connection, synchronization, local readiness and diagnostics.

## Build and test on Debian

Use Debian 13 x86-64, Rust 1.100.0 with rustfmt and clippy, and Node 26.5.0 with npm 11.17.0. Install build-essential, pkg-config, libssl-dev and libdbus-1-dev as for Desktop, plus libudev-dev for gamepad support and clang, llvm-dlltool and lld-link for the prerequisite supervisor. Python 3 and python3-vdf are needed for the Decky tests. python3-pil is only needed for the M3.0 fixture suite. The compiled plugin needs no Node, Rust, compiler or Python packages on the Deck.

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

To run the real Server fixture with the packaged build on Debian, build Server with its pinned Rust 1.88.0. For the X11 extension install xvfb, xauth, libxkbcommon-x11-0, a software Mesa GL stack, dbus-x11 and gnome-keyring. Use a disposable session bus and keyring. Install xdotool for keyboard interaction in the wrapper fixture. The following command runs the M3.0 HTTP suite as well as the Decky backend setup, authenticated sync, artwork, GUI download, reload and display-free offline Play.

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
/usr/bin/python3 -B decky/package.py   --binary target/release/app-depot --out "$out" --development
(cd "$out" && sha256sum -c SHA256SUMS)
desktop-file-validate "$out/AppDepot-Decky-Installer.desktop"
~~~

The ZIP contains AppDepot/plugin.json, package.json, main.py, dist/index.js, bin/app-depot and LICENSE. Packaging uses fixed entry times, ordering and modes. The versioned installer script pins the exact ZIP URL and SHA-256. It verifies the ZIP before unpacking, rejects foreign paths, non-regular archive entries and unexpected identities, and revalidates the hash in the privileged activation step. Installation locks the Decky plugin directory and stages on the same filesystem. Updates atomically exchange the two directories and roll back on a failed service restart. The backend runs as the Steam user. A healthy Decky service restart does not prove frontend activation. [Decky installer service and directory conventions](https://github.com/SteamDeckHomebrew/decky-installer/tree/e42a3c828de90673959b4de26cf5c5d2c7830554)

For the public beta, download only the `AppDepot-Decky-Installer.desktop` asset from the applicable [official Decky prerelease](https://github.com/viktorliho/app-depot-client/releases). In Desktop Mode, open the download in Dolphin and use **Properties → Permissions** to enable execution. Close Properties, double-click the installer and confirm. Enter the Desktop Mode password if sudo prompts. Wait for the success message, then return to Gaming Mode and check the version under **Decky → App Depot → Settings & About**.

The dedicated Decky workflow builds public beta tags using `decky/package.py --github-release-tag TAG`. It publishes the ZIP, compact desktop entry, versioned Python installer and `SHA256SUMS` as a GitHub prerelease. It compares all four artifacts across two packaging runs, verifies that the desktop entry pins the script URL and digest and that the script pins the ZIP URL and digest, and checks that the Desktop latest release still has the same ID.

The desktop entry keeps only a short bootstrap in `Exec` to keep the command within Dolphin's practical limits. Dolphin may reserialize the entry when its permissions change. The bootstrap downloads the versioned script over verified HTTPS, allows a single redirect to GitHub's release-asset host, limits script size, and checks the SHA-256 embedded in the desktop entry before running any downloaded code. The script stays in memory through verification and execution. The script's ZIP downloader applies the same host policy and supports resumption. App Depot Server traffic follows the Rust core's separate redirect policy. For development builds, keep the desktop entry, versioned installer script and ZIP in the same directory. The launcher verifies both local files offline. Installation failures are recorded in `~/AppDepot-Decky-Installer.log`.

For installer acceptance on SteamOS, download the published desktop entry using a graphical browser, enable execution in Dolphin Properties, then launch it from Dolphin. Confirm installation, enter the sudo password if prompted, observe download and verification progress and the success dialog, then confirm the new version in Gaming Mode. A local build with official URLs needs the matching assets at those exact release URLs before it can complete this test.

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

## First Play in Gaming Mode

For Windows First Play, select an installed Wine or Proton runtime in Settings & About. Open the existing shortcut and review the runtime, transfer size and prerequisite list in the handheld wrapper. Navigate with the D-pad or left stick, confirm with A and use B to go back or cancel. LB and RB scroll long reviews. Arrow keys, Enter, Escape, Page Up and Page Down provide keyboard equivalents. Native applications use the same review and progress window. The wrapper shows Core transfer observations, measured network speed and an ETA when enough data is available. File preparation reports the current entry. Resume reuses durable checkpoints. A pending installation asks for retry consent. Core binds the application to its own prefix, runs declared installers and launches the active generation. The native shortcut remains the Steam Input target.

## Shortcut setup and launcher diagnostics

In SteamOS multi-Xwayland sessions, Steam and the game must use the same game display. If the assigned local X socket is unavailable, Play records `steam_game_display_unavailable` and Decky shows recovery instructions. Close any separate test compositor before switching to Desktop Mode and back to Gaming Mode. Start hardware acceptance from the real Steam shortcut. A nested headless Gamescope session can reserve the display number that Steam expects when Gaming Mode starts.

Steam on SteamOS 3.9.5 derived the initial name from the executable and left launch options unset on the two observed beta.2 creations. It also stored a second pair of quotes around the quoted executable it received. The installed Steam UI calls AddShortcut with the name, executable, arguments and full command line. The adapter now passes those fields in that order, with an unquoted executable and a quoted executable within the command line, saves the returned AppID and explicitly sets missing shortcut fields after readback. A legacy entry is eligible only when the saved AppID, expected launcher alias and exact malformed live and disk fields agree. Setup is resumable and stops if unrelated metadata changes. Artwork starts after correct metadata and ownership are confirmed. Steam writes its own VDF.

Inspect the latest native launch record at `$XDG_STATE_HOME/app-depot-decky/last-launch.json` or, when the environment variable is unset, `~/.local/state/app-depot-decky/last-launch.json`. The record contains stage, PID and exit code. It contains no credentials, command line or Server name. The record is written when the launcher starts. An unchanged record after Play is evidence to inspect Steam's executable and launch options before diagnosing transfer or game failures.

## M3.1b hardware acceptance

1. Record SteamOS, Steam client and Decky versions. Start with Decky installed. In Desktop Mode permit execution of `AppDepot-Decky-Installer.desktop`, launch it, confirm the operation and verify the restart succeeds. Return to Gaming Mode and check that App Depot opens in Decky. Repeat installation to check idempotence. Keep the Deck online until the pinned GitHub package finishes downloading.
2. Verify beta.1 profiles and tokens survived the upgrade. Open **Discover Servers** and choose the known advertisement. If discovery fails, enter the original Server address manually. Try Guest, or enable **Use an access token** if Guest is prohibited. For HTTP, enable **Allow unencrypted HTTP**. Test, then save and synchronize. Guest only lists Public Published items. Confirm Unreal Gold and Unreal Tournament 1999 through a profile authorized to see them. Do not change their visibility for this test.
3. Record actual shortcut IDs and set a custom name, artwork and Steam Input layout on a disposable App. Sync twice, synchronize two equally named Apps or two profiles, and rename an App at the Server. Check persisted IDs, user settings, artwork and absence of duplicate/foreign changes. Restart Steam, reload Decky and repeat. Try an upgrade while preserving library and bindings. Verify whether icon and artwork changes need a restart, including logos without a prior custom position.
4. Choose an undownloaded native App. Use the Deck controls for any choice and observe transfer phases. Cancel during download, then Retry. Confirm checkpoint resume and launch. Stop an uninstalled Play while its Server is offline and confirm Retry/Exit. Restore connectivity and Retry. Start an installed App offline and check it opens without a download window.
5. Check the native game's sustained focus, built-in controls, Steam Controller if available, Steam Input layouts, overlay, Stop Game, normal exit, descendant processes, playtime and return to Steam. Confirm no transfer process remains after cancellation and no game remains after Stop Game. Check that the parent waits while the game runs.
6. Check Steam artwork in portrait and wide slots before and after download, verify transparency and other roles, then change Server artwork and sync. Check persistence after Steam restart and preservation of a user override. Confirm a missing art role never prevents shortcut creation.
7. Start with internal storage. With owner approval test an existing library split-root configuration on a microSD fixture, including card removal and return, before offering a location switch in Decky. The MVP displays the selected shared library location. Alternate-root UI is deferred. Test actual SteamOS binary loading and Secret Service availability in Gaming Mode separately.

Debian verification covers the real Server, native X11 under Xvfb, Secret Service, an offline launch without a display, child exit code and SIGTERM propagation, mocked SteamClient and VDF readback, and package extraction/activation rollback with a fixture service. The hardware cases above are still open.

### Interrupted package downloads

The Desktop installer shows received bytes, transfer speed and the current installation phase in its terminal. A single run can resume an interrupted download using a strong ETag and a validated HTTPS Range response. A full response to a resume request starts a fresh transfer. Without a strong validator the installer retries from byte zero. Interrupted attempts use capped exponential backoff.

The connection and TLS setup timeout is 20 seconds, and socket reads allow 30 seconds without progress. The overall operation is bounded by five attempts and 128 MiB per response, with no elapsed-time cutoff for a healthy transfer. The installer checks the complete ZIP against its embedded SHA-256 and validates the archive before activation. A cancelled installer run does not leave a partial package on disk.

## Manual installation and updates

1. Open the official Client GitHub Releases page and choose the newest App Depot **Decky** release. The default Desktop release channel is separate. Download its `AppDepot-Decky-Installer.desktop` asset. [Official releases](https://github.com/viktorliho/app-depot-client/releases)
2. In SteamOS Desktop Mode, permit execution and launch the installer. It verifies the versioned installer script and ZIP, then replaces the App Depot plugin and restarts Decky.
3. Return to Gaming Mode and open **Decky → App Depot → Settings & About** to confirm the installed Decky version. The panel links to the release page. Diagnostic details list the Rust core version separately.

Manual Desktop Mode installation is the supported update policy. Gaming Mode offers the installed version, release link and diagnostics. App Depot does not check for new plugin versions, stage update packages or activate new versions in Gaming Mode.

## SteamOS MVP status

Owner acceptance on real Steam Deck hardware covered the official Desktop installer, Gaming Mode frontend and backend, Server connection and Guest access, catalog synchronization, stable Steam AppIDs and artwork assignment. Unreal Gold and Unreal Tournament 1999 showed grayscale artwork before native First Play downloaded, verified and launched the content. Subsequent Play launched directly. Later owner testing confirmed local Uninstall preserves the shortcut and returns the app to the not-downloaded state, ready for the next installation. The existing Steam non-Steam shortcuts remain the library interface, with grayscale grids before download and color after installation. Decky **Installed** provides local Uninstall, while Steam Play manages native launch.

Owner acceptance also confirmed Overlord II installation, interactive Windows prerequisites in Gamescope and automatic launch with Proton Experimental. Controller navigation and progress in the updated wrapper require separate handheld review.

## Deferred Gaming Mode update research

M3.4 tested signed release manifests, protected paths and atomic replacement with recovery and rollback on Debian. M3.4.1 inspected actual NonSteamLaunchers script updates, Decky-managed installation, user-space runtime separation and Loader's path trust boundary. These are historical feasibility investigations. None is part of the supported Decky update policy. [M3.4 implementation and fixture](https://github.com/viktorliho/app-depot-client/commit/385065c2e8f9353600d778e4dba27b8c27dd171d) · [M3.4.1 decision and evidence](https://github.com/viktorliho/app-depot-client/commit/6987094eeecdbe84db4192fa9a3545a345ecfd1b)

### M3.4 privileged-helper research

This section preserves the earlier design as reference. Desktop Mode is the supported update path.

Prefer a supported Decky transaction with equivalent guarantees if one becomes available. For Loader versions without it, the candidate design is an opt-in, socket-activated system service restricted to the explicitly enrolled Deck user. The Desktop installer would explain the added authority and install the root-owned executable, units, signing public key and enrollment together after explicit administrator approval. The socket must live below a root-owned directory with access restricted to the enrolled account. The helper must verify the caller UID via kernel peer credentials. Authenticated UID bounds which account may request installation. It does not distinguish the Decky plugin from another process running as that user, so the protocol must remain safe for any process with that UID. Restrict the service to the socket, staging, transaction state and App Depot destination. It needs neither direct Internet access nor access to the game library. [systemd socket activation](https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html) · [systemd service sandboxing](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)

- **Authority.** Expose status and one activation request for App Depot. Accept only a bounded, signed release manifest and ZIP bytes via IPC. Pin the repository, plugin identity, exact version/channel, package digest, size and archive layout in the signed contract. Require a release signature under a production key pinned at enrollment, monotonic eligible versions, and a separately reviewed key rotation procedure. GitHub HTTPS and asset digests used by the preview do not authorize privileged activation. No caller-supplied destination, executable, service name, script, arguments or environment reaches a privileged operation.
- **Staging and trust.** Read the upload into exclusive root-owned storage, verify the signed manifest and digest there, and extract those same bytes through directory-relative operations. Refuse links, traversal, special files, unexpected entries and excessive size. Open and validate root-owned directories without symlink traversal and hold pinned directory handles across verification and activation. Loader must resolve the same trusted destination on reload. A root-owned leaf under user-replaceable ancestors is insufficient for this guarantee. Confirm a supported protected path or reject activation on that layout. Extract plugin files as inert data and load the plugin only under its existing unprivileged Decky identity. The release schema must reject privilege-escalating plugin metadata.
- **Coordination.** Serialize helper transactions and hold an exclusive lease spanning replacement and validation. Introduce a shared lease acquired by App Depot launch/transfer entry points before they enter the affected lifecycle and held through game exit or transfer completion. Defer activation while the lease is busy and block new launch startup while the exclusive lease is held. Test this with installed launchers that predate the lease, and require a validated bootstrap transition before activation can be enabled. Avoid a whole-Steam restart. Reload App Depot through a validated Loader operation or require a separately confirmed idle Decky restart if no isolated reload exists.
- **Recovery.** Persist a root-owned transaction journal and fsync the package directories and state before and after atomic exchange. Retain a previous generation until a bounded, generation-specific health check confirms the expected version is serving normal RPC through Loader. Treat this check as a liveness signal, never as an integrity check. On timeout, failed reload or interrupted exchange, identify the actual on-disk generation, restore the previous one, reload it and report the outcome. Recovery must handle a crash at each durable transition. The previous generation is eligible only for recovery initiated by the in-flight transaction.
- **Removal.** Offer a separately confirmed Desktop Mode removal flow. Wait for or recover any active transaction, disable and remove only the updater's units, enrollment, executable and updater-owned staging, then reload systemd. Leave the Decky plugin, user library, bindings and credentials in place.

Before any implementation is installed on a Deck, a disposable Linux harness must exercise peer authorization, package tampering, symlinks and traversal, concurrent requests, a running game holding the lease, requests arriving during activation, health timeout and crashes before and after exchange. A Loader-specific harness and a separately approved Deck test must then demonstrate that the enrollment, intended reload operation and rollback work on the supported runtime. No privileged helper is shipped by this milestone.

### M3.4 SteamOS activation gate

A read-only check on SteamOS 3.9.5 with Decky Loader 3.3.1 found `/home/deck` at mode 0700 and `/home/deck/homebrew` at mode 0755, both owned by `deck`. The `plugins` and `AppDepot` leaves are root-owned. On restart, Loader resolves its configured plugin path by name again. Pinning a descriptor during replacement does not bind that subsequent resolution. The root systemd service also executes `/home/deck/homebrew/services/PluginLoader`. Both the executable and the plugin path need controlled ancestry before App Depot can initiate a privileged reload. The fixture verifier rejects any non-root-owned, group-writable or world-writable ancestor. [Loader 3.3.1 plugin path](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/main.py#L47)

Loader 3.3.1 recognizes `PRIVILEGED_PATH` and `UNPRIVILEGED_PATH`. Its internal backend reload RPC queues a reload and does not return generation-specific health. A Loader-controlled, root-trusted installation layout, including the Loader executable and service configuration, is the first gate. Verify that layout in isolated SteamOS before designing or authorizing an enrollment on the owner's Deck. App Depot does not change those paths, enroll a helper or expose a Gaming Mode activation control. The existing Desktop installer remains available. [Loader Linux path configuration](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/localplatform/localplatformlinux.py#L238) · [Loader backend reload](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/loader.py#L235)

### Debian development fixture

`decky/update_security.py` verifies a bounded canonical manifest and a raw Ed25519 signature through OpenSSL. The signature covers schema, plugin identity, official repository, beta channel, exact tag and version, the canonical ZIP filename, byte size, SHA-256, x86-64 architecture, exact package layout and plugin API compatibility profile. The verifier supplies OpenSSL sealed descriptors and a fixed environment. It requires a pinned DER Ed25519 public key and rejects a same-version or older candidate. The package is still subject to the normal archive validator. The test key is a public RFC 8032 test vector. [Ed25519 test vector](https://www.rfc-editor.org/rfc/rfc8032.html#section-7.1)

`decky/update_transaction_fixture.py` runs only with a disposable protected directory named `/run/app-depot-update-fixture-*`. It exercises the signature gate, protected staging, atomic exchange, single transaction lock, shared session lease, durable journal and rollback. A prepared journal without a healthy generation restores the old directory. A recorded healthy generation survives recovery. The reload adapters in the tests simulate success, failure or interruption and compare the expected version, digest and fixture generation. This is not a Loader RPC, privileged service, enrollment implementation or actual Steam session detector. Kernel peer authentication is exercised separately on a local fixture socket. The ordinary Decky ZIP carries none of the fixture modules or test keys.

Build the local review bundle after compiling the Decky CLI and frontend. It uses an offline Desktop installer and creates a signed test manifest plus checksums. The installer does not request enrollment. Run root fixture tests only on a disposable Debian development environment.

~~~sh
cargo build --release -p app-depot --features decky --locked
npm --prefix decky ci
npm --prefix decky run build
python3 -B tools/steamos/build_update_review.py --binary target/release/app-depot --out /path/to/local/review
sudo -n /usr/bin/python3 -I -B -m unittest discover -s decky -p test_update_security.py -v
~~~

### Production signing and first activation test for the archived helper design

The official workflow continues publishing the ordinary ZIP, installer and checksum. Its Python test step covers manifest verification with the public fixture key. Before publishing activation-capable assets, separately authorize a protected Ed25519 signing identity and enroll its public key through an independently trusted Desktop installer. Restrict the signing operation to reviewed release inputs after the existing version, annotated-tag, source, reproducibility and package checks. Record one digest per signed version. Publish the canonical manifest and detached signature only after verifying them against the enrolled public key. Keep signing outside untrusted jobs and pull requests. Review key rotation separately and authenticate a replacement through an already trusted key or fresh, explicitly authorized enrollment. No production key was generated or enrolled for the fixture.

The first hardware acceptance gate is a separately authorized, disposable SteamOS test with both Loader paths protected and a supported reload and generation-health signal. Verify the Loader launches only the intended generation before testing activation. Bootstrap a launcher version that participates in the lease, close old launchers and confirm no pre-bootstrap session is running before enabling updates. With a disposable game and test profile, verify that an active game or download defers activation, normal Play is protected during exchange, a healthy candidate stays installed and a failed acknowledgement restores the old plugin. Recheck AppID, artwork, library, credentials and offline launch. Do not enroll the public fixture key on the owner's Deck.

## M3.4.1 Decky-managed update feasibility

**Decision B: additional platform changes required.** Keep App Depot's existing official HTTPS release verification and Desktop Mode installer. The candidate investigated in M3.4.1 used a Loader-managed install and Loader's confirmation UI after App Depot verified the exact release. App Depot will not add a root daemon, privileged IPC, production key enrollment or a second transaction engine for this approach.

### Loader evidence

The Deck reports SteamOS 3.9.5, Decky Loader 3.3.1 and App Depot 0.1.0-beta.6. Both `PRIVILEGED_PATH` and `UNPRIVILEGED_PATH` are `/home/deck/homebrew`. The plugin directory and AppDepot are root-owned 0755. The parent directories under the user's home are owned by `deck`. The root unit executes `/home/deck/homebrew/services/PluginLoader`, observed root-owned with mode 0777. The executable itself is writable by the desktop account. Upstream's Loader binary updater also applies mode 0777 and replaces the old binary before the service resumes. Moving just App Depot's directory, or changing only the executable's mode, leaves an untrusted path component. A supported protected placement has to survive Loader and SteamOS updates. [Loader binary updater](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/updater.py#L182) · [Linux path settings](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/localplatform/localplatformlinux.py#L238)

The inspected stable tag and upstream main were both `ffd103b9c1a4ce5bf305e7c0998eda6e05457dad`. The current prerelease `v4.0.0-pre1` was also inspected at `24082fe63cdcdf41993f9e1430d1405152a2c930`. Loader's Store install and owner confirmation are existing product operations. The underlying `utilities/install_plugin`, `utilities/confirm_plugin_install` and `loader/reload_plugin` routes are internal interfaces. The active versions download the asset, remove the previous plugin, then verify its SHA-256 and extract. Backend reload queues a request and does not acknowledge healthy startup. The open upstream transactional staging PR is a relevant contribution point. Its proposed behavior is not available in either tested revision. [Loader install code](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/browser.py#L173) · [Loader reload code](https://github.com/SteamDeckHomebrew/decky-loader/blob/ffd103b9c1a4ce5bf305e7c0998eda6e05457dad/backend/decky_loader/loader.py#L235) · [Loader PR #987](https://github.com/SteamDeckHomebrew/decky-loader/pull/987)

### NonSteamLaunchersDecky reference

The installed NSL package was 1.4.04. Its bundled frontend confirms that Update calls Python `install` with `up=true` and independently schedules a Steam restart after five seconds. Python makes the shell script executable, temporarily changes X server access controls, runs the script with the `Up` argument via the shell (optionally through xterm), and interprets the exit code. The installed backend runs at UID 1000 with no effective capabilities. Its manifest has the debug flag, and the `Up` branch contains no privilege escalation or Loader installation request.

The shell compares version strings against mutable main, uses a mutable branch ZIP, attempts to remove and recreate the NSL directory, extracts and copies through fixed temporary paths, and changes permissions. It prints that hot reload was triggered and exits without verifying a digest or awaiting the new backend. The branch has no automatic rollback and can continue after command failures. The frontend suggests the Desktop installer for recovery. The Linux Desktop NSL installer is a separate privilege path and uses a sudo prompt to prepare a user-owned plugin tree. Such a tree can explain updates on other installations. On this Deck both the NSL directory and its parent are root-owned 0755. Some existing NSL files are user-owned, so the backend can potentially write individual entries after changing permissions. It cannot replace the entire directory. No NSL update was executed during this audit. [NSL frontend at 5bca453](https://github.com/moraroy/NonSteamLaunchersDecky/blob/5bca453e24fb0ef192c0786dd3854f8b4256de51/src/index.tsx#L80) · [NSL backend at 5bca453](https://github.com/moraroy/NonSteamLaunchersDecky/blob/5bca453e24fb0ef192c0786dd3854f8b4256de51/main.py) · [NSL shell at 0a0d4f0](https://github.com/moraroy/NonSteamLaunchers-On-Steam-Deck/blob/0a0d4f0cd0a99ae1d9a1c958440846cce28ae4db/NonSteamLaunchers.sh) · [NSL Desktop installer at 0a0d4f0](https://github.com/moraroy/NonSteamLaunchers-On-Steam-Deck/blob/0a0d4f0cd0a99ae1d9a1c958440846cce28ae4db/NSLPluginInstaller.sh)

### Alternatives and beta compromise

| Approach | Trust and filesystem boundary | Lifecycle and recovery |
| --- | --- | --- |
| **A. Decky-managed install** | Use the exact official HTTPS release URL and SHA-256 validated by App Depot. Loader must independently bind downloaded bytes to the authorized version and destination. Prefer a pinned URL over a user-writable local file. A trusted Loader executable and plugin root are prerequisites. A compromised App Depot process can lie about its own validation, so App Depot's preflight alone cannot authorize the handoff. | Preserve library, profiles, credentials and bindings outside the install tree. Check idle state before the Loader prompt and prevent new conflicting operations through handoff. Use Loader reload rather than a timed Steam restart. A beta can use a best-effort previous-version backup and a separately obtained, digest-pinned Desktop recovery installer, provided Loader verifies before deleting. Current versions cannot meet that minimum. |
| **B. Separate helper** | The M3.4 fixture validates signed metadata, caller UID and root path ancestry. A production service would additionally need a protected Loader, trusted enrollment and service lifecycle. A compromised backend with the same UID must be treated as an arbitrary requester. | The fixture covers exchange, leases and rollback. Real games, Loader reload, upgrade and removal of a service remain separate integration problems. This design is archived. |
| **C. Versioned user runtime** | A fixed unprivileged plugin could verify a versioned runtime into the user's data directory. Root code must never execute or import from it. A compromised backend can modify runtime data after verification, within that same user's authority. A signed package alone does not elevate trust in the user directory. | A user-level generation pointer can be switched back. Keep active game binaries pinned and protect shared state across versions. Runtime cleanup must avoid active versions. Decky frontend, backend and bootstrap changes still need a Loader-managed update. This adds two version contracts without solving full plugin installation. |
| **D. Desktop Mode installer** | The existing version-pinned installer verifies SHA-256 and the exact ZIP structure before installing. Privilege is requested for the explicit install. This remains the recovery distribution. On the observed device, restarting its existing Loader unit still shares the platform executable-path risk. | With games closed, it stages on the same filesystem and rolls back a failed activation callback. Service status is not plugin health and sudden interruption lacks Loader-managed recovery. Package upgrades preserve external data. A previous official installer can recover the plugin independently of Gaming Mode. |

The minimum upstream change is a Loader install path that verifies the exact candidate before destructive work, exposes supported confirmation and plugin-local reload, and runs from protected executable and plugin paths. Once those conditions hold, a beta can accept manual recovery instead of introducing an independent daemon or automatic transaction engine. Recovery uses a known-good official Desktop installer obtained before the experiment. App Depot should present a failed or unconfirmed reload as such and keep its existing shortcut and content data outside plugin replacement. The current named paths and executable mode are not acceptable for this test.

### Verification and first hardware gate

The unprivileged replay executes the pinned Loader methods with local fixture data and fake network, install and backend adapters. Both revisions preserve separate data and install a correctly hashed package after confirmation. Both remove the previous generation before rejecting a bad digest, leave it absent after an injected startup failure, and return from reload while the generation is still queued. The M3.4 root fixture remains available for independent signature, unauthorized peer, lease, SIGKILL, rollback and recovery regressions. Neither fixture makes calls to the owner's Loader.

~~~sh
python3 -B tools/steamos/check_loader_activation.py /path/to/decky-loader --loader v3.3.1
python3 -B tools/steamos/check_loader_activation.py /path/to/decky-loader --loader v4.0.0-pre1
~~~

If Gaming Mode update research is resumed under a separate milestone, first authorize a **disposable** SteamOS test environment using a Loader-supported protected layout. Scope the test to installing and restoring one disposable App Depot plugin after leaving games and downloads idle. Verify the executable and plugin ancestry in read-only mode first. Only then exercise Loader's own confirmation, a verified package, plugin-local reload and independent Desktop recovery. The current Deck requires a separate platform repair proposal and approval before any service or filesystem changes. No hardware activation commands are approved by this milestone.

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
