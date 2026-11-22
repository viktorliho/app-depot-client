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

Steam on SteamOS 3.9.5 derived the initial name from the executable and left launch options unset on the two observed beta.2 creations. It also stored a second pair of quotes around the quoted executable it received. The adapter now passes the raw executable and directory to creation, saves the returned AppID and explicitly sets missing shortcut fields after readback. A legacy entry is eligible only when the saved AppID, expected launcher alias and exact malformed live and disk fields agree. Setup is resumable and stops if unrelated metadata changes. Artwork starts after correct metadata and ownership are confirmed. Steam writes its own VDF.

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
