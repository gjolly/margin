# Rune Diff

A Git diff viewer based on Rune Kit's diff example (MIT). Rune and Rune Kit
are pinned to the same Git revision; Cargo.lock and flake.lock pin dependencies.

## Build and run on NixOS

From this project:

    nix develop -c cargo build --release --locked
    nix develop -c cargo run --release --locked

The viewer runs Git in its current working directory. To inspect another
repository, enter the development shell and invoke the binary there:

    nix develop
    cargo build --release --locked
    export PATH="$PWD/target/release:$PATH"
    cd /path/to/repository
    rune-diff
    rune-diff --cached
    rune-diff HEAD~1 HEAD
    rune-diff main...HEAD -- src/
    rune-diff --no-index old.txt new.txt

Arguments are passed directly to Git, preserving the -- path separator and
shell-quoted paths. With no arguments, this shows tracked working-tree changes
against the index, just like git diff. Untracked files are not included.
Split view is the default; the toolbar can switch to unified view.
Use the release build for interactive use: the default debug build leaves Rune's
layout, text shaping, and rendering code unoptimized and can scroll slowly.

The viewer controls patch formatting (no colors, external diff helpers or
textconv). Output-only modes such as --name-only, --raw, --check and --output
are rejected. Rune demo flags such as --scene and --snap are no longer accepted.
Git runs once after the first frame; this is a snapshot, with no automatic refresh.
Errors and empty results appear in the window. Text patches must be UTF-8;
binary changes show Git's binary-file summary.

## UI zoom and navigation

Ctrl++ (or Ctrl+=) enlarges the whole UI; Ctrl+- shrinks it.
Zoom changes in 10% steps from 50% to 200%; Ctrl+0 resets to 100%.
Zoom lasts for the current session. j/k navigate hunks; ]/[ navigate files.

## Inline AI conversations

Hover a text line's gutter and click the blue **+** to open an inline composer.
You can also select a range using the line numbers, then click its **+**.
Type a question and click **Send** or press **Ctrl+Enter**. Enter inserts a newline.
A small spinner stays below your comment while the AI works. Completed replies
appear below it; send another message in the same composer to continue.
Drafts and threads survive scrolling and switching views, but are **session only**:
closing the application discards them. Clicking **+** on the same range reuses its thread.

Create `$XDG_CONFIG_HOME/rdiff/config.json`, or `~/.config/rdiff/config.json` when
`XDG_CONFIG_HOME` is unset, empty, or relative. Configuration is loaded at startup;
restart the app after changing configuration or environment variables.

```json
{
  "ai": {
    "base_url": "https://api.openai.com/v1",
    "model": "your-model-id",
    "api_key_env": "OPENAI_API_KEY",
    "headers": {},
    "extra_body": {},
    "timeout_seconds": 180
  }
}
```

Set the named environment variable in the environment used to launch rune-diff.
Alternatively, set `"api_key": "your-token"` within `ai`; a nonempty environment
token takes precedence. Neither credentials nor configuration files are created
by the app. With missing configuration or credentials, the composer keeps your
draft and explains setup, with sending disabled. Diff viewing still works.

`model` is required and is sent exactly as configured. `base_url` defaults to the
OpenAI URL above; `/chat/completions` is appended, so include any provider-specific
base path such as `/v1`. The token is sent with Bearer authentication. This version
requires a token and an **OpenAI-compatible Chat Completions endpoint with function
tool support**, even for local providers. Native Responses, Anthropic, and other
protocols are not supported. Provider compatibility has not been verified against
live services.

`headers` accepts additional string-valued HTTP headers. `extra_body` accepts
arbitrary JSON request fields, for example `{"max_completion_tokens": 4096}` or
provider-specific reasoning parameters. Parameters are sent unchanged; the
provider validates them. The app owns `model`, `messages`, `tools`, `tool_choice`,
`stream`, and `n`; overriding these is an error. Authorization, Host, Content-Type,
and Content-Length headers cannot be overridden. URLs must use HTTP(S), without
embedded credentials, queries, or fragments. Requests do not follow redirects.
`timeout_seconds` applies to each HTTP request including its response body and
accepts 1–3600 seconds. Answers arrive as complete messages rather than streaming.

Each conversation gives the selected code priority, followed by up to 12 patch
rows on either side, comparison arguments, and a small changed-file overview.
The AI is instructed to answer your question directly and factually, focusing on
the selected lines, with wider context available through read-only tools.
The AI can call read-only tools to list files, search literal text, and read line
ranges. Tool access is rooted at the repository root, or the launch directory
outside Git. Tools read **current local working-tree files**, which can differ
from staged or historical code in the displayed patch. Ignored files, `.git`,
and symlink targets outside the eligible code tree are excluded. The AI cannot
run shell commands or modify files. For `--no-index`, files outside the tool root
remain available through their displayed patch context only.

Limits keep work and retained data bounded: 64 threads per session, four working
threads at once, one pending turn per thread, 64 KiB initial context, 32 KiB per
tool result, and 512 KiB serialized history per thread. Each turn allows at most
16 model requests and 64 tool calls; each HTTP response is capped at 2 MiB. Code
reads inspect at most the first 1 MiB of each file and return up to 400 lines.
Listings page through 200 files; searches return at most 100 matches and scan at
most 64 MiB. Inventory and search work each have a ten-second scan budget; the
inventory is capped at 100,000 visited entries or 8 MiB of paths. Truncation and
errors are reported explicitly. Failures offer **Retry**, which resends the pending
turn without adding another user comment; there are no automatic retries.

## Development environment

The Nix shell supplies Rust, Git, the C linker, font libraries, Wayland/X11
libraries and the Vulkan loader. It configures library lookup, including the
NixOS host GPU drivers. Run from a desktop session with working Vulkan drivers.
The shell supports x86-64 and ARM64 Linux; only the setup host has been tested.

    nix develop -c cargo test --locked

## Distribution

The Nix development binary references the Nix store. For Ubuntu distribution,
build this Cargo project in Ubuntu CI or a container and package its runtime
dependencies. Ubuntu packaging is not configured yet.

## License

MIT; see LICENSE for the upstream attribution retained with the example.
