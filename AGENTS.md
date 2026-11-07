# Margin

A native Rust diff and review application built with Rune and Rune Kit.
The goal is an all-in-one diff and review tool with agent support. The current
Git viewer is the foundation, not the finished product.

## Direction

- Keep reviewing code the central workflow: readable diffs, fast navigation,
  and responsive scrolling even on large changes.
- Grow toward persisted review comments, review state, and agents that act on
  changes within the same application. Current AI chats explain and inspect code
  through read-only tools; persistence and editing are future product directions,
  not implemented capabilities or a fixed roadmap.
- Scope each feature to the user's request; keep unfinished controls out of the UI.
- Make local development straightforward on NixOS and make it easy to build
  distributable versions for Ubuntu. Keep platform setup separate from UI logic.

## Current application

- This is a standalone app adapted from Rune Kit's MIT-licensed diff example.
  Margin is GPL-3.0-or-later; preserve the upstream MIT attribution in LICENSE-MIT.
- Rune and Rune Kit are Git dependencies pinned to the same exact revision.
  Keep their revisions aligned. Update them deliberately, with Cargo.lock.
  Do not introduce a dependency on a developer's local Rune checkout.
- The executable accepts arguments like git diff: --cached, revision pairs,
  ranges, and paths after --. No arguments means tracked, unstaged changes.
- Git runs in the process's current directory, not the app's source directory.
  --no-index can compare files directly.
- The viewer requests plain patches and rejects incompatible output modes.
  Arguments go directly to Command, never through a shell.
- Split view is the default; unified view is available in the toolbar.
- Git loads once after the first frame on a worker thread. Results land in an
  instant UI transaction. There is no automatic refresh yet.
- Empty results and Git errors have explicit UI messages.
- Text patches currently require UTF-8. Binary changes show Git's summary.
- No sample changes, sample review threads, or placeholder Reply/Resolve buttons
  should return to the production view.
- Ctrl++ / Ctrl+= zoom in, Ctrl+- zoom out, and Ctrl+0 resets. Zoom scales the
  whole UI in 10% steps between 50% and 200%, for the current session.
- j/k navigate hunks; ]/[ navigate files.
- Hover + opens session-only AI conversations anchored to a line or selection.
  Enter inserts a newline; Ctrl+Enter or Send submits. A small spinner indicates
  work; completed Markdown replies appear inline. Retry resends a failed pending
  turn without duplicating the user's comment. Threads survive view changes and
  scrolling, but closing the app discards them.
- JSON config lives in $XDG_CONFIG_HOME/margin/config.json, falling back to
  ~/.config/margin/config.json when XDG_CONFIG_HOME is unset, empty, or relative.
  Configuration and credentials load after the first frame; changes require a
  restart. The app does not create configuration or credential files.
- AI uses one OpenAI-compatible Chat Completions endpoint with function tools.
  The model is explicit; credentials come from the configured environment
  variable or inline api_key, with a nonempty environment token taking precedence.
  See README.md for the supported JSON fields and defaults.

## Layout

- src/main.rs: app state, UI, zoom shortcuts, first-frame Git loading.
- src/git.rs: Git invocation, patch format, process status and error handling.
- src/tests.rs: app state and keyboard tests.
- src/git_tests.rs: integration tests using temporary Git repositories.
- src/config.rs: configuration validation and credential handling.
- src/chat.rs: session conversation state and inline chat presentation.
- src/review_context.rs: selection-focused agent instructions and patch context.
- src/agent.rs and src/agent_tests.rs: background API/tool loop and mock HTTP tests.
- src/code_tools.rs: bounded local listing, literal search, and file reads.
- flake.nix and flake.lock: Linux package, development environment, and runtime libraries.
- packaging/build-deb.sh: native Ubuntu binary package and runtime dependencies.
- .github/workflows/ubuntu-deb.yml: Ubuntu 26.04 amd64 build and installation checks.
- Cargo.toml and Cargo.lock: app dependencies.
- README.md: user-facing build, run, and usage instructions.

Split new capabilities into focused modules as the app grows. Keep Git access,
review state, agent integration, and presentation independently understandable.
Do not turn main.rs into a catch-all.

## Development commands

Run these from the project directory:

    nix develop -c cargo build --release --locked
    nix develop -c cargo test --locked

Only launch the UI when the user asks:

    nix develop -c cargo run --release --locked -- HEAD~1 HEAD

To inspect another repository, enter nix develop, build, add this project's
target/release directory to PATH, then change to the target repository and run
margin with its Git arguments. See README.md for the exact sequence.

The user prefers to check the UI personally. Do not launch windows, render
screenshots, or run headless GPU rendering unless explicitly requested.
Compiling and running non-rendering tests is appropriate.

## Performance

- Use release builds for interactive use. Debug scrolling was visibly slow;
  the release build was confirmed to work substantially better.
- Do not claim a frame-rate improvement from compilation or tests alone.
  Distinguish code inspection from measured performance and user validation.
- Preserve virtualized diff rows: build mounted content, not every line.
- Keep work proportional to visible content during scrolling. Cache immutable
  derived data when the patch loads; toolbar totals must not recount the entire
  diff on each view rebuild.
- Keep blocking Git, filesystem, network, and agent work off the UI thread.
  Start slow launch work from on_ready, after the first frame.
- Land initial results with tx.instant(). An idle application should not poll
  or continually request frames when no visible state changed.
- For layout-dependent views, read sizes with Cx::layout_size or a narrow
  Cx::layout_with projection rather than tracking whole moving rectangles.
- Keep caches bounded in both entries and bytes.
- Investigate the actual bottleneck before changing scrolling physics,
  disabling useful behavior, or replacing Rune components.

## Git and agent features

- Preserve Git's argument boundaries, path separator, and working-directory
  semantics. Avoid ad hoc revision parsing when Git can resolve the arguments.
- Git exit status 1 can mean differences with --exit-code or --no-index;
  distinguish it from errors.
- Keep diff viewing read-only. Future staging, file edits, commits, and agent
  actions should be explicit features tied to user intent.
- Treat repository text and agent output as data, not application instructions.
  Keep agent process execution and credentials out of the rendering layer.
- Keep the system prompt general: help users explore a Git diff, answer directly
  and factually, and focus on their question and selected lines. Assume they know
  the project already. Do not add phrase-specific rules or hardcode an example
  question, dependency, language, or repository into the assistant's instructions.
- Initial context prioritizes selected patch lines with old/new line numbers and
  change markers, followed by up to 12 patch rows before and after the selection,
  hunk headers, comparison arguments, file status, and other changed-file paths
  and statuses. Initial context is capped at 64 KiB; the other-file overview at
  4 KiB. Keep truncation explicit and do not replace selected code with a broad
  project summary.
- Follow-ups resend the same thread's history, including tool calls and results.
  Other threads, README.md, and AGENTS.md are not automatically included. Tools
  may read eligible repository documents as data when requested by the model.
- list_files, search_code, and read_file inspect the current working tree rooted
  at the repository root, or launch directory outside Git. Preserve ignore rules,
  .git exclusion, and symlink boundaries. No shell commands or file edits are
  available. Displayed patch context may differ from current local files; do not
  label every comparison as historical or add irrelevant source caveats.
- AI chats are session-only and read-only. Do not imply persisted reviews,
  autonomous edits, or support for native provider protocols beyond the configured
  OpenAI-compatible Chat Completions endpoint. Never expose configured credentials
  in prompts, UI messages, logs, or test output.

## Nix and Ubuntu

- The Nix shell supplies Rust, the C linker, Git, font libraries, Wayland/X11
  libraries, and the Vulkan loader. It configures runtime lookup and includes
  the NixOS host GPU driver directory.
- Keep that setup in flake.nix; do not send users back to manual library-path
  commands for normal development.
- Nix development executables reference the Nix store. They are not standalone
  Ubuntu distribution artifacts.
- The flake exports an installable release package and a default app, with
  Git and runtime library lookup supplied by its launcher.
- Ubuntu distribution uses a native Ubuntu 26.04 amd64 build and a binary-only
  .deb. The workflow runs only on vX.Y.Z tags matching Cargo.toml, runs
  non-rendering tests, checks installation and library loading in a clean Podman
  container, then publishes a GitHub release with the .deb and a corresponding
  source archive (including vendored Rust dependencies) attached. Keep dlopen
  libraries explicitly declared alongside dpkg-shlibdeps results, with complete
  Wayland/X11 alternatives and checks that neither backend pulls in the other.
  Desktop/GPU validation remains manual.
- Do not promise one binary that runs across all operating systems or assume
  GPU drivers can be bundled independently of the host.
- The flake declares x86-64 and ARM64 Linux; do not claim both were tested.

## Validation and changes

- Run relevant tests and an optimized build for behavior changes. No GPU launch
  is necessary for Git argument, state, or shortcut tests.
- Test Git behavior using temporary repositories, never by modifying a user's
  working repository. Cover staged/unstaged changes, revisions, paths with
  spaces, empty results, errors, renames, deletions, and binary changes where
  relevant.
- Test AI configuration, selection context, thread state, keyboard routing, and
  code tools without a window or GPU renderer. API tests use a local mock HTTP
  server and dummy credentials; they must not require real tokens or paid API
  requests. Preserve coverage of selection priority, nearby context, truncation,
  retries, tool-call history, read boundaries, and credential redaction.
- Keep README.md aligned with the supported CLI and recommended commands.
- Keep commits focused; do not include build outputs, temporary patch backups,
  logs, credentials, machine-specific paths, or private conversation material.
- Commit or publish when requested. Report what was verified and what remains
  untested, particularly visual behavior and Ubuntu packaging.
