# Agent governance for App Depot Client

## Authority and scope

- The owner’s current task sets immediate scope. This repository’s current architecture, local-library, lifecycle, milestone and conformance documents govern client design. The App Depot Server’s OpenAPI 3.1, API semantics, architecture and Tree Identity v1 documents are the source of truth for the network protocol and Server-held declarations. This client must not invent endpoints, mandatory fields, executable semantics or a competing protocol.
- Read applicable contracts and inspect implementation evidence before changing behavior. A new prompt does not silently replace an accepted contract; an explicit owner task can authorize a specific pre-1.0 change. Material contradiction between authoritative documents is a Hard Stop until resolved.
- The client is an optional reference implementation. Its core must be independent of any Server deployment, TrueNAS, Steam, Decky and UI framework. Other languages and clients may implement the protocol directly.

## Pre-1.0 compatibility

- Before an owner-declared stable release, client APIs, local schemas and configuration may change within an expressly authorized task; compatibility is not implied. Do not add obsolete wrappers or migration complexity solely to preserve earlier pre-1.0 internals.
- A breaking local-library change requires a documented migration, backup/restore or explicit fresh-library action. Never silently abandon downloaded bytes, jobs, credentials or user choices. Security, integrity, recovery and data durability remain mandatory.
- Server API/declaration changes require a separate authorized Server task and coordinated Server implementation, OpenAPI, documentation and tests. Client convenience alone is not permission to edit the Server.

## Authorized autonomy

- Within an expressly authorized client task, make ordinary local engineering choices, fix bounded defects, run tests against isolated data, update relevant documentation and make coherent local commits. Do not use that authority for unrelated features, significant new dependencies, architectural replacement, server changes, remote creation, deployment or publication.
- Keep Download, Install, runtime preparation and Launch distinct. Never infer execution/install behavior from names, extensions, executable bits, embedded payload data or scans. Declarations and downloaded content are untrusted; Server metadata is not a security authorization mechanism.
- Do not add arbitrary script execution or automatic system package installation under a general client task. Those need a specific milestone, shared declarative contract, security review and user-authority design.

## Git publication policy

- After completing an explicitly authorized development task, agents MAY create coherent verified local commits and MUST push them to the configured `origin/main` without separate permission, provided every required check has passed, no Hard Stop remains, and the current owner instruction does not prohibit push. This standing authorization covers ordinary fixes within that task, not unrelated work. A task-specific no-push instruction takes precedence; M0 has such an instruction. No agent may create or change a remote merely to satisfy this policy.
- Before any allowed push, verify working/staged status, branch/upstream and configured fetch/push URLs; fetch and verify synchronization with `origin/main`; review every outgoing commit and full diff for task scope, secrets, credentials and disposable artifacts. If `origin/main` advances, integrate without discarding others’ work, rerun affected checks and review again. Never force-push, rewrite remote history, bypass protection or publish unverified work.
- An explicitly assigned Desktop release task authorizes the full release lifecycle without further approval for each step: version and notes, required Linux and Windows CI, commit and push to main, annotated tag, official platform builds, verified draft, stable publication as GitHub Latest, and independent public asset verification. Routine commits never authorize a new release. One shared version, immutable annotated tag, source commit and complete stable release are required. Existing published releases and tags are immutable. macOS becomes mandatory after owner acceptance. Decky publication and other deployments remain separately authorized.
- Local commits required by an owner task remain local when that task forbids push.

## Repository and documentation hygiene

- Keep only current source, required assets/manifests, necessary tests/fixtures, and durable developer/product documents. Never commit progress reports, temporary plans, diagnostics, logs, screenshots, disposable scripts, credentials, caches or build outputs. Keep disposable review data outside the repository.
- Documentation describes the current project in English. Update an existing authoritative document instead of appending stale history or making a duplicate. Add a new document only for a distinct durable contract. Do not claim planned UI, install or launch behavior as implemented.
- Prefer the smallest complete change. Each abstraction needs a demonstrated responsibility. Avoid speculative frameworks, extension points, managers, compatibility switches and broad refactors. Remove superseded code when refactoring. No client-specific convention may quietly become a protocol rule.

## Client architecture and local work

- Preserve distinct Server-profile, App, Release, association, Artifact and declaration-revision identities. Preserve unknown declaration JSON on read and write; typed projections cannot silently drop it. Core protocol, domain, transfer and persistence code must not depend on egui, Steam, Decky or TrueNAS.
- Verify SHA-256 and Tree Identity v1, validate all paths and destination representability, and commit downloads atomically. Refuse unsafe paths, symlinks and ambiguous materialization. No payload inspection may invent missing software semantics. Unsupported declarative operations remain visible but inert.
- Use separate Dev/Test client libraries, credentials and Server configurations. Never point automated tests or reset helpers at real user libraries or Production Server data. Do not infer destructive targets from the current directory. Do not change host-wide configuration or unrelated user files during routine work.
- Secure tokens belong in platform secure storage; never in source, logs or ordinary local-library records. A fallback policy must be explicit and must not silently store plaintext secrets.

## Hard Stop

Stop blocked implementation when: a task conflicts with an accepted contract without explicit replacement authority; authoritative documents materially conflict; behavior has materially different interpretations that evidence cannot resolve; a Server contract change is needed without authorization; download security or integrity would be weakened; a local-library change risks corruption or user-data loss without an approved recovery path; a required filesystem guarantee cannot be established; required verification is impossible after reasonable investigation; prior verification proves invalid; repository state cannot be reconstructed reliably; work exceeds authority; or remote publication/destructive work is needed without authorization.

On Hard Stop, preserve the working tree and useful work. Do not hide it in a WIP commit or bypass checks. Keep diagnostics outside the repository. Report exact file/line evidence, consequence, smallest viable options, Git status, checks completed and work unfinished, then wait for owner direction. Ordinary test failures are investigated and fixed within authorized scope; they are not automatically a Hard Stop.

## Verification and commits

- A task is complete only when the requested behavior and documentation match, applicable tests pass, affected prior checks still pass, and required local commits are made. Test against the real Server implementation with isolated Dev/Test data where network behavior matters. Never claim skipped checks as passed or disable tests merely to get green.
- Before each commit, review full diff and staged paths for scope, accidental contract changes, generated output, secrets and incomplete behavior; run formatting and relevant tests. Make coherent verified commits, not routine WIP commits. Do not rewrite history to conceal an error.
- Capability and readiness claims must match implemented behavior. In particular, a verified download is not an installed or launchable application.

## Reorientation

At the start of substantial work and after context loss, reread this file and relevant client and Server contracts; inspect Git status, recent commits, implementation and tests. Reconstruct outstanding work from repository evidence, not a prior agent summary alone. Investigate uncertainty and use Hard Stop if reliable reconstruction is impossible.
