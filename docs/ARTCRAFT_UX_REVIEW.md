# ArtCraft Launcher review

Inspected [`storytold/craft-launcher`](https://github.com/storytold/craft-launcher) at commit `7c613b4ccf901f395b15e5601fb7b2f01073dd37` (Rust workspace, desktop and web apps, `crates/core`, `crates/native`, `crates/ui-egui`, packaging and screenshots). Its [README](https://github.com/storytold/craft-launcher/blob/main/README.md) describes grid/table browsing, progress, install/update/open and native packages.

| Pattern | ArtCraft observation | App Depot adaptation |
| --- | --- | --- |
| Core/UI separation | Core release/platform/status models, native backend and egui UI are separate crates; the UI receives backend state and queue updates. | Keep HTTP protocol, transfer and local library out of egui; CLI and GUI call the same core. |
| Grid and table | Screenshots and `crates/ui-egui/src/{views,app}.rs` provide a visual grid and sortable table, search and details. | Share one catalog query/read model across views. Show every Release/association rather than a curated fixed suite. |
| Queue and progress | `crates/native/src/queue.rs`, UI job state and progress separate waiting, active and resumed work. | Persist jobs across restarts, show per-file and aggregate progress, distinguish download verification from installation. |
| Install/update status | `crates/core/src/status.rs`, `crates/ui-egui/src/model.rs` distinguish available, installed, update and work states. | Use explicit fact dimensions: download, integrity, install and launch support. Updates follow explicit channel/selection, not guessed version order. |
| Details/artwork | `crates/ui-egui/src/details.rs` shows release facts, imagery and action context. | Show Server App artwork, raw/typed declarations, all Artifact choices and unsupported reasons. Missing art gets a neutral placeholder. |
| Platform/package handling | `crates/core/src/platform.rs` parses Craft asset filenames and chooses GitHub release packages for OS/CPU; native code installs specific package formats. | **Do not copy** filename parsing, GitHub release selection or auto-installer policy. App Depot requires Server declarations and currently lacks install/launch semantics. |
| Settings/packaging | Desktop settings and platform packaging live outside core; Linux, Windows and macOS packaging scripts are explicit. | Start Linux native packaging after M1 core works; design paths and credential adapters for all three OSes. |

ArtCraft is a useful UX and Rust boundary reference, not an App Depot protocol source. Its fixed application catalog and GitHub asset naming rules would violate Server-authoritative declaration semantics here.

The desktop uses ArtCraft's broad navigation and grid/table/details/queue separation as UX guidance. Its first-run flow instead handles arbitrary Server origins and private catalogs. App cards and local Library entries lead with names and optional art; identifiers and raw declaration JSON are available in Advanced sections. A persistent profile cache appears before network refresh, and every Release association remains a separate choice. The implementation keeps command/event workers over the existing core, native Wayland/X11 windowing and bounded asynchronous artwork. ArtCraft's updater, GitHub release picker, installer and branding are not used.
