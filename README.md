# App Depot Client

Native reference client for the open [App Depot Server](https://github.com/viktorliho/app-depot-server) protocol. The client is optional: independent clients can implement the documented Server API without using this code.

This repository currently contains the M0 architecture and a small Rust workspace foundation. No desktop application, installer or launcher is implemented yet. See [milestones](docs/MILESTONES.md) and the [server API audit](docs/SERVER_API_AUDIT.md).

The client treats Server declarations and downloaded content as untrusted. A download never implies permission or ability to install or execute it.
