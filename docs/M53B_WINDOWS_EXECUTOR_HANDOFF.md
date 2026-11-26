# M5.3B Windows executor handoff

**Historical handoff at commit 34df141.** The branch subsequently adopted observed installer exit as the completion result for UAC launches. The blocker and test results below describe the handoff revision. Client base is `637f53b17ac34c3859daa784f105a2dd7f8d0853`. The server contract is `docs/installation-manifest-v1.md` at `2ed0c5bc176136c7f4087132d992425893b465a8`.

## Implementation and remaining blocker

The regular CLI and the Windows Desktop worker supply Core's `NativeExecutor` to the existing prerequisite coordinator. The coordinator owns consent, source verification, exit-code policy, retry and generation activation. The adapter binds the prepared x86/x64 PE, supplies its parent directory as cwd, and quotes each literal argument. Ordinary execution creates the process suspended, assigns it to a job before resume, waits for the root, and waits for the job to drain.

On error 740, `windows_process::authorized_elevation` uses the Windows runas verb for the bound image. Only the installer requests elevation. Cancellation maps to `Cancelled`. On approval, Windows returns a handle to an already running root. The adapter cannot establish descendant membership from the start. Every accepted elevated execution therefore returns `Ambiguous`, including root exit 0. The journal records the observed root exit in the detail and leaves the structured exit code empty. The generation stays pending and its attempt workspace is retained. Root exit alone must not authorize activation or cleanup. Another attempt requires fresh plan-bound retry consent.

## Evidence on the Windows VM

| Location | Evidence |
| --- | --- |
| `C:\personal\dev\fixtures\m50-windows-prerequisites` | M5.0-W sources `fixture.rs` and `fixture-x86.c`, binaries, `research.md`, `results-x86.txt`, and `run-f283c31bbc3b4199b5144751ddd839a0\results.txt`. Prototype observed x86 under x64, literal argv, signed exits including -1, 259, 3010 and 1641, and a descendant surviving root exit. |
| `C:\personal\dev\fixtures\m50-windows-prerequisites\m50-w1` | W.1 sources and binaries for layout, GUI and token-elevation fixtures. See `evidence-w1.md`, `results-w1.txt`, `results-exit-policy.txt`, `results-elevation-direct.txt` and `results-elevation-runas.txt`. Prototype observed direct launch error 740, elevated token and exit 0 through runas, supporting-file/cwd failure 41, GUI cancellation exit 1602 and delayed descendant completion. |
| `C:\personal\dev\fixtures\m53b1-prepared` | Prepared source layout seeds `seed.py` and `seed-tree.py`, prior driver and integrity evidence. For product acceptance the seeds populated disposable libraries; process execution used the regular CLI. |
| `%TEMP%\m53b-8eb561fc-c592-4974-98a2-98d2b5724973` | Product CLI installed, confirmed the fingerprint and selected prerequisite, processed it to Ready, played, uninstalled, reinstalled and reached Ready again after fresh consent. Layout fixture checked two supporting files, Unicode path, cwd, whitespace, a quoted literal and an empty argument. |
| `%TEMP%\m53b-uac-wqb0equ8` | Product CLI recorded Cancelled with no active generation. A user clicking No in a genuine UAC prompt was not independently confirmed. A subsequent attempt exposed a mixed-separator path error, fixed on this branch. |
| `%TEMP%\m53b-uac-q8t77k51` | After path normalization, the self-built elevation fixture, staged as layout.exe, checked its elevated token and returned 0. Journal recorded Ambiguous and PrerequisitesPending. |

These directories are local to the Windows VM. In the acceptance runs, `%TEMP%` was `C:\Users\vikto\AppData\Local\Temp`. The fixture binaries and disposable libraries are outside Git.

## Verification boundary

On Windows 11 x64, Rust 1.100.0 passed `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked`. The final log at `%TEMP%\m53b-final-tests.log` records 103 Core and 25 Desktop tests passed, with five ignored across test targets. Desktop worker tests used a mock executor. Interactive Desktop end-to-end acceptance, a product-level x86/failure-code matrix, elevated descendant supervision and hosted Linux/Windows CI remain unverified. The prototype observations above are separate from the product results. The existing CI push filter covers main and feature/**; pushing this handoff branch does not establish a CI result.
