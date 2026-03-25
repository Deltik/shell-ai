# Changelog

[![GitHub releases](https://img.shields.io/github/release/Deltik/shell-ai.svg)](https://github.com/Deltik/shell-ai/releases)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.6.2 (UNRELEASED)

### Added

- **Custom curl binary for HTTP requests (`curl_cmd` / `SHAI_CURL`)**

  Shell-AI can use an external curl-compatible binary instead of its built-in HTTP client. This enables [curl-impersonate](https://github.com/lexiforest/curl-impersonate) for bypassing TLS fingerprint-based bot detection (Cloudflare), custom TLS configurations, client certificate authentication, and other transport-level needs that the built-in client doesn't support.

  The command is parsed using POSIX shell quoting rules, so extra arguments work naturally:

  ```bash
  export SHAI_CURL='curl-impersonate --proxy socks5://localhost:1080'
  ```

  Or set persistently in `config.toml`:

  ```toml
  curl_cmd = "curl-impersonate"
  ```

- **Automatic runtime detection of libcurl-impersonate**

  If `libcurl-impersonate.so` is installed on the system (or loaded via `LD_PRELOAD`), Shell-AI detects it automatically and uses it for HTTP requests. No configuration needed; the detection checks for the `curl_easy_impersonate` symbol to distinguish it from regular libcurl.

  Example:

  ```shell
  deltik@box53 [~]$ shell-ai suggest -- 'get boot time in UTC'
  Error: No suggestions could be generated.
  Reason: API error: HTTP 403: {"error":{"message":"Access denied. Please check your network settings."}}
  ```
  
  ```shell
  deltik@box53 [~]$ LD_PRELOAD=/tmp/curl-impersonate/build/curl-8_15_0/lib/.  libs/libcurl-impersonate.so.4.8.0 CURL_IMPERSONATE=firefox147 shell-ai suggest -- 'get boot time in UTC'
  Select a command:
    [1] date -u -d @$(($(date +%s) - $(awk '{print int($1)}' /proc/uptime)))
     2  date -u -d "$(who -b | awk '{print $3\" \" $4}')" +"%Y-%m-%d   %H:%M:%S %Z"
     3  date -u -d @$(($(date +%s) - $(awk '{print int($1)}' /proc/uptime)))   +"%Y-%m-%d %H:%M:%S UTC"
     g  Generate new suggestions
   n  Enter a new command
   q  Quit
  
  ↑↓/jk navigate • key/Enter select • Esc quit
  ```

- **Network error retry with exponential backoff**

  Transient network errors (connection refused, DNS failures, TLS errors) are now retried with exponential backoff, matching the existing retry behavior for HTTP 429 rate limiting. Configuration errors (like a missing `curl_cmd` binary) fail immediately without retry.

### Changed

- **Overhauled CLI help pages for standalone usability**

  The help output is now designed to be self-sufficient without consulting external documentation:

  - `explain` subcommand description no longer mentions "OpenAI-compatible API" — now reads "Explain a shell command in plain language"
  - `--provider`, `--frontend`, and `--output-format` are now proper `ValueEnum` types, so clap validates input and lists possible values in help
  - Usage examples added to `suggest`, `explain`, `config`, `integration`, and `integration generate` (visible in `--help` but not `-h`)
  - `config` help now explains that bare `shell-ai config` shows the active configuration
  - `config init` help now shows the platform-specific config file paths and mentions legacy JSON config precedence
  - Tip about the `shai` shorthand shown in `--help` (conditionally hidden when `shai` is already in PATH)
  - `-h` (short) vs `--help` (long) now meaningfully differ: `-h` is concise, `--help` includes examples, expanded descriptions, and value explanations

- **Restructured CLI option grouping**

  AI configuration overrides (`--provider`, `--model`, `--max-tokens`, `--temperature`, `--frontend`, `--preview-mode`, `--locale`) are now shown under a "Configuration Overrides" heading and only appear on subcommands that use them (`suggest`, `explain`, `config`). Utility subcommands like `config schema`, `config init`, `integration list`, etc. no longer show irrelevant AI options.

  `--output-format` and `--debug` remain available on all subcommands.

  **Breaking:** Configuration overrides must now be placed _after_ the subcommand, not before it. For example, `shell-ai --provider openai suggest` must be written as `shell-ai suggest --provider openai`. The `shai` shorthand is unaffected.

- **Binary name is no longer hard-coded**

  Help text, usage examples, integration scripts (aliases, keybindings, completions), error messages, and generated config headers now use the actual executable name at runtime. Renaming, copying, or symlinking the binary makes all output reflect that name. The only exception is the `shai` shorthand, which always activates suggest mode regardless of the binary name for backwards compatibility with [ricklamers/shell-ai](https://pypi.org/project/shell-ai/).

### Fixed

- **Legacy JSON config with environment variable keys now works**

  The JSON config file (`config.json`) inherited from [ricklamers/shell-ai](https://github.com/ricklamers/shell-ai) uses environment variable names as keys (e.g. `OPENAI_API_KEY`, `SHAI_API_PROVIDER`). These keys were silently ignored since v0.5.0. They are now remapped to the internal config paths so both legacy env-var-style keys and TOML-style keys work. When both styles set the same setting, env-var-style keys take precedence.

## v0.6.1 (2026-03-23)

### Added

- **Automatic correction for APIs that don't support structured outputs** ([#1](https://github.com/Deltik/shell-ai/issues/1))

  Some OpenAI-compatible or Anthropic-compatible API providers (e.g., at the time of writing, [cloud models via Ollama](https://github.com/ollama/ollama/issues/12362), GitHub Copilot proxies) silently ignore the `response_format` JSON schema constraint, causing the model to return Markdown-wrapped JSON or non-conforming output. Shell-AI now detects invalid responses and automatically retries with correction feedback, building a multi-turn conversation that steers the model toward valid output.

  Invalid response prefixes such as Markdown fences are detected on the first streaming token, aborting the request early to avoid wasting time on a response that can never conform.

  Up to 2 correction attempts are made before giving up. Note that using APIs without structured output support will add latency and token costs (if applicable) from the extra round-trips.

### Fixed

- **Merged system messages into one for OpenAI-compatible backends** ([#3](https://github.com/Deltik/shell-ai/pull/3))

  Models served through Jinja-based chat templates (e.g., `qwen3.5` via Ollama) only accept a single system message. Multiple system messages are now merged with `\n\n` separators before sending, matching the existing Anthropic backend behavior. Credit to [@alanjds](https://github.com/alanjds) for the contribution.

- **Tolerate missing `children` key in explanation JSON** ([#3](https://github.com/Deltik/shell-ai/pull/3))

  Models that don't support structured outputs may omit the `children` array instead of returning `[]`. The parser now treats a missing key as an empty list. The key remains required in the model-facing schema to encourage deeper explanations from capable models. Inspired by [@alanjds](https://github.com/alanjds).

- **Arrow keys moved two options at a time on Windows** ([#2](https://github.com/Deltik/shell-ai/issues/2))

  On Windows, crossterm reports both key press and key release events. The UI event loops now filter for press and repeat events only, fixing double-input on Windows while preserving key-repeat behavior.

## v0.6.0 (2026-02-12)

### Added

- **Live streaming previews for suggestions and explanations**

  Watch AI responses materialize in real-time. As suggestions generate, they appear token-by-token in a dedicated preview pane. This tighter feedback loop lets you see results faster before they're all ready.

  - `shell-ai suggest`: See all your suggestions stream in simultaneously, each in its own slot. Pick the one you want the moment it looks right.

    [![shell-ai suggest -- 'perl: Animate 5 seconds of an indeterminate progress bar in the style of a six-color ANSI rainbow wave at 24 frames per second'](docs/images/suggest-danish.gif)](docs/images/suggest-danish.gif)

  - `shell-ai explain`: See the breakdown stream directly into your terminal as the AI reasons through the command.

    [![shell-ai explain -- 'git rebase -i --autosquash --autostash origin/main && git rebase --autostash --committer-date-is-author-date origin/main'](docs/images/explain-git-rebase.gif)](docs/images/explain-git-rebase.gif)

  The preview adapts to your terminal: If there's not enough room for all the content, it gracefully truncates while preserving structure.

- **New `preview_mode` setting** to control maximum preview display density

  - `minimal`: Single line progress indicator (similar to v0.5 behavior)
  - `compact`: Add a preview pane with a line per item with truncation
  - `full` (default): Multi-line previews with full content

  Configure via:
  - CLI: `--preview-mode=minimal`
  - Environment: `SHAI_PREVIEW_MODE=compact`
  - Config file: `preview_mode = "minimal"` in `~/.config/shell-ai/config.toml`

  > [!TIP]
  > Set to `minimal` for an experience closer to Shell-AI v0.5.

### Changed

- **Switched to streaming API responses** for all providers (OpenAI, Anthropic, Claude Code).

  Previous versions waited for the entire response before displaying results. Now tokens stream as they arrive, enabling the live preview feature. This also means you can interrupt (Ctrl+C) early if you see a good result.

- **API keys are now optional** for OpenAI, Anthropic, Groq, and Mistral providers, so the `api_key` option may be left empty.

  This allows using compatible APIs like Ollama that don't require authentication. API keys are still needed for the official provider APIs.

  The API base URL (`api_base`) is what's actually required, although defaults are already present.

- **Exiting suggestion menu now preserves the display** when using `frontend = "dialog"`

  Pressing `q`, `Esc`, or `Ctrl+C` to exit the suggestion menu now leaves the suggestions visible for reference instead of clearing them from the terminal.

- The dialog menu (`frontend = "dialog"`) help line now uses color to distinguish keys from actions.

- **`shell-ai explain` now encourages explaining before summarizing** to improve the accuracy of the summary.

- **Ctrl+G shimmer animation pauses briefly between cycles** instead of wrapping continuously so that it's less attention-grabbing. The wave highlight now fades in from the left, traverses the text, fades out to the right, and rests for two dark frames before repeating. This applies to all shell integrations (Bash, Zsh, Fish, PowerShell).

### Fixed

- **Dialog menu line counting now handles Unicode correctly**

  Previously, suggestions containing Unicode characters (emoji, CJK, etc.) could cause the menu to miscalculate its height, resulting in cursor positioning errors during navigation. Line counting now uses display width instead of byte length.

- **Dialog mode now supports 10+ suggestions**

  When `suggestion_count` exceeds 9, suggestions beyond the 9th are displayed with `?` as their shortcut key. Navigate to them with arrow keys and press Enter to select.

- **"Revise command:" text input no longer breaks when input exceeds terminal width**

  The previous implementation rendered on a single terminal line using `MoveToColumn`. When the prompt and input together exceeded the terminal width, cursor positioning and line clearing broke — `MoveToColumn` past the width pushed the cursor off-screen, and `Clear(CurrentLine)` only cleared one physical line, leaving ghost text from wrapped content on every keystroke.

  Text input now renders through the same virtual buffer and diff-based rendering engine used for streaming previews. Content wraps naturally across multiple terminal rows with the cursor correctly tracked at the edit point within the wrapped area. When wrapped content would exceed the terminal height, the editor falls back to a horizontal scroll mode with `…` overflow indicators. The terminal understands the wrapped lines as a single logical line, so text selection and reflow work correctly.

- **Anthropic provider now uses native structured outputs** instead of tool use for JSON schema enforcement. The previous approach did not guarantee the model would use the tool, potentially returning unstructured responses. The `output_config.format` API guarantees schema-compliant output.

- `Error: API error: Claude CLI failed with exit code 1` when using `provider = "claudecode"` and Claude Code is not configured in verbose mode. The `claude` command at the time of writing requires the `--verbose` option to be set when using the streaming output mode.

- Text input now handles multi-byte Unicode correctly. The cursor, word boundaries, backspace, delete, and kill operations now track char indices instead of byte offsets. Cursor positioning uses display width for correct alignment with CJK and other wide characters.

- Terminal raw mode is now restored on panic. A RAII `Drop` guard ensures `disable_raw_mode()` runs even if the interactive select or text input panics, preventing the terminal from being left in a broken state.

- `shell-ai explain` no longer panics when truncating multi-byte UTF-8 content. The `truncate_to_limit` function now finds a valid char boundary instead of slicing at a byte offset.

- Context buffer trimming no longer panics on multi-byte UTF-8 output. The context buffer that captures the last 1500 characters of command output now finds a valid char boundary instead of slicing at an arbitrary byte offset.

- `shell-ai config` no longer panics on non-ASCII API keys when masking values for display. The mask now correctly finds the last 6 characters by Unicode code point instead of byte index.

## v0.5.3 (2026-01-29)

### Added

- **Anthropic Claude API provider** (`provider = "anthropic"`)

  Native support for the Anthropic Messages API. Set `ANTHROPIC_API_KEY` or configure under `[anthropic]`.

- **Claude Code CLI provider** (`provider = "claudecode"`)

  Use [Claude Code](https://docs.anthropic.com/en/docs/claude-code) as a backend. No API key needed; Claude Code manages its own auth. Optional settings under `[claudecode]`.

### Changed

- **Backend abstraction layer**

  Internal refactor: providers now implement a `Backend` trait, separating HTTP-based providers from subprocess-based ones. No user-facing changes.

## v0.5.2 (2026-01-11)

### Added

- **Locale-aware AI responses**

  Shell-AI now auto-detects your system locale from `LANG`/`LC_ALL` environment variables and hints the AI to respond in your preferred language. This applies to both `suggest` and `explain` commands.

  Configure with:
  - `--locale <value>` CLI flag
  - `SHAI_LOCALE` environment variable
  - `locale` setting in config file

  Set to an empty string (`locale = ""` or `SHAI_LOCALE=""`) to disable locale hints.

- **Multi-line command support for Ctrl+G keybinding**

  The keybinding integrations for all shells (Bash, Zsh, Fish, PowerShell) now support multi-line command output, in case the generated suggestion spans multiple lines. Previously, only the first line was used.

### Fixed

- **Bash keybinding integration: first argument no longer ignored**

  Fixed a bug where the Bash keybinding (Ctrl+G) would corrupt the command line, causing the first word to be lost. For example, `uptime -s` would display correctly but execute as two separate commands (`uptime` then `-s`).

  The root cause is a Bash bug where `$(< file)` corrupts `READLINE_LINE` when used in a `bind -x` context. The workaround uses `$(cat file)` instead, which forks a subprocess and avoids the corruption.

  This bug is not present in Bash 5.3.

- **Dialog frontend: multi-line commands now display correctly**

  Fixed rendering issues when suggestions contained newlines. The menu now properly handles carriage returns and calculates the correct number of terminal lines, preventing display corruption when navigating between options.

## v0.5.1 (2025-12-22)

### Added

- **New `shell-ai integration` subcommand for shell integration management**

  Generate shell integration files with configurable features:
  - `shell-ai integration generate <shell>` – Generate integration for bash, zsh, fish, or powershell
  - `shell-ai integration update` – Regenerate all installed integrations (preserves preferences)
  - `shell-ai integration list` – Show available features and installed integrations

  **Presets** control which features are included:
  - `minimal` – Tab completions only
  - `standard` (default) – Completions + aliases (`??`, `explain`)
  - `full` – Completions + aliases + Ctrl+G keybinding

  **Customization** with `--add` and `--remove` modifiers:
  ```bash
  shell-ai integration generate zsh --preset standard --add keybinding
  shell-ai integration generate fish --preset full --remove aliases
  ```

  Integration files are written to `~/.config/shell-ai/integration.<ext>` with embedded preferences for future updates.

- **New `automatic` frontend mode (now the default)**

  The `frontend` setting now defaults to `automatic`, which intelligently selects the appropriate frontend based on context:
  - TTY + human output → `dialog` (interactive menu)
  - Non-TTY + human output → `noninteractive` (prints first suggestion)
  - JSON output → `noninteractive` (prints all suggestions as JSON)

  This makes `--output-format=json` work seamlessly without needing to explicitly set `--frontend=noninteractive`.

### Changed

- **Mutual exclusion validation for `frontend` and `output_format`**

  JSON output (`--output-format=json`) now requires a compatible frontend. Combining JSON output with an explicitly-set interactive frontend (`dialog` or `readline`) is now a configuration error with a helpful message. Use `frontend=automatic` (default) or `frontend=noninteractive` with JSON output.

- **Optimized API calls in noninteractive human output mode**

  When using `--frontend=noninteractive` with human output format (the default), only 1 suggestion is now generated instead of `suggestion_count` (default 3), since only the first suggestion is used. JSON output mode still generates all suggestions for programmatic selection.

## v0.5.0 (2025-12-20)

### Added

- **Single-binary distribution**
  
  Complete rewrite in Rust for improved performance and single-binary distribution (no Python interpreter required)

- **`explain` subcommand**

  `shell-ai explain` is the inverse of `shell-ai suggest`. It breaks down shell commands with AI-powered explanations, optionally augmented with man page citations.

- **`shell-ai config` subcommand** for configuration management:

  - View current configuration with source annotations (CLI, environment, TOML, JSON, or default)
  - Sub-subcommand `schema` lists all available settings
  - Sub-subcommand `init` generates an annotated config template
  - Validation errors include hints pointing to the exact source
  - Values can be native types or strings (e.g., `temperature = 0.5` or `temperature = "0.5"`)

- **TOML configuration file** as the preferred format:

  - Linux: `~/.config/shell-ai/config.toml`
  - macOS: `~/Library/Application Support/shell-ai/config.toml`
  - Windows: `%APPDATA%\shell-ai\config.toml`

- **Multiple frontend modes** with the `--frontend` option or `SHAI_FRONTEND` environment variable:

  - `dialog` (default, arrow key navigation)
  - `readline` (text-based), and
  - `noninteractive` (scripting)

- **JSON output format** via `--output-format=json` for all subcommands

- **`--debug` option and `SHAI_DEBUG` environment variable** for debug and trace logging to stderr

  Use `--debug` for debug level, `--debug=trace` for trace level, or set `SHAI_DEBUG=debug` or `SHAI_DEBUG=trace`

- **HTTP retry** logic with exponential backoff for rate limits (429) and server errors (5xx)

- **Progress spinner** with elapsed time display during API requests

- **Vim-style keybindings** (j/k) in addition to arrow keys for menu navigation

- **Number shortcuts** (1-9) for quick selection in dialog mode

- **Action menu** with clipboard (c), explain (e), execute (x), revise (r), and back (b) options

- **Readline-style text input** with standard keybindings (Ctrl+A/E, Ctrl+U/K, word navigation)

- `--provider`, `--model`, `--temperature`, `--max-tokens`, and `--frontend` CLI flags for runtime overrides

- **`SHAI_MAX_TOKENS` environment variable** to limit maximum tokens per AI completion

  Optional; when omitted, the API auto-calculates the limit. Provider-specific variables (`OPENAI_MAX_TOKENS`, `GROQ_MAX_TOKENS`, etc.) are also available for per-provider control.

- **`max_reference_chars` setting** to control man page context size in `explain` (default: 262144)

- **Standard HTTP proxy support** via `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` environment variables

### Changed

- CLI structure now uses subcommands: `shell-ai suggest`, `shell-ai explain`, `shell-ai config`
- `shai` command is now shorthand for `shell-ai suggest` (detected via program name)
- Provider must now be explicitly configured (no default provider)
- API responses now use JSON Schema enforcement for guaranteed valid structured output
- `SHAI_SKIP_CONFIRM=true` now translates to `--frontend=noninteractive` internally

### Deprecated

- **Context mode** (`--ctx` flag and `CTX` environment variable): The extra context from shell output tends to confuse the completion model rather than help it. Kept for backwards compatibility but not recommended.
- `SHAI_SKIP_CONFIRM` environment variable: use `--frontend=noninteractive` or `SHAI_FRONTEND=noninteractive` instead
- JSON configuration format (`config.json`): TOML format (`config.toml`) is now preferred

### Removed

- Python runtime dependency and all Python packages (langchain, InquirerPy, openai, groq, mistune)
- Environment variable `SHAI_SKIP_HISTORY=false` mode, which wrote executed suggestions to the shell history

  The original implementation made assumptions about how a user configured their shell, and shells do not pass this information to child processes, so the use case is infeasible to support.
- `OPENAI_PROXY` environment variable (use `api_base` config option for OpenAI-compatible proxy endpoints, or standard `HTTP_PROXY`/`HTTPS_PROXY` for network proxies)
- `OPENAI_API_TYPE` environment variable (replaced by `SHAI_API_PROVIDER`)
- `DEBUG` environment variable (replaced by `SHAI_DEBUG` or `--debug` flag)

## v0.4.4 (2025-08-27)

### Added

- Mistral AI API provider support with `MISTRAL_API_KEY`, `MISTRAL_MODEL`, and `MISTRAL_API_BASE` configuration
- OpenAI-compatible API endpoint support via `OPENAI_API_BASE` environment variable

### Changed

- `SHAI_API_PROVIDER` now accepts `mistral` and `ollama` as options
- Default Ollama model changed to `phi3.5` with default API base `http://localhost:11434/v1/`

## v0.4.3 (2024-12-25)

### Added

- Ollama API provider support with `OLLAMA_MODEL`, `OLLAMA_MAX_TOKENS`, and `OLLAMA_API_BASE` environment variables
- Ollama configuration example in README

### Changed

- `OPENAI_API_KEY` documentation clarified to indicate it can be left empty when using Ollama

## v0.4.2 (2024-12-20)

*Internal refactoring only, no user-facing changes.*

## v0.4.1 (2024-12-13)

### Added

- Groq API provider support as an alternative to OpenAI and Azure
- `SHAI_TEMPERATURE` environment variable to control output randomness (default: 0.05)
- Configuration file support at `~/.config/shell-ai/config.json` with default values and proper fallbacks
- Parallel suggestion generation using ThreadPoolExecutor for faster response times

### Changed

- Default API provider changed from OpenAI to Groq
- `SHAI_API_PROVIDER` environment variable replaces `OPENAI_API_TYPE` for provider selection
- Configuration system now merges user config with sensible defaults
- Updated dependencies: langchain 0.3.0, langchain-openai 0.2.0, openai 1.57.0, groq 0.13.0

## v0.3.26 (2024-06-03)

### Fixed

- Handling of missing OS info fields on Linux systems to prevent errors when certain platform identifiers are not available

## v0.3.25 (2024-05-24)

### Added

- Auto-save executed commands to shell history (supports zsh, bash, csh, tcsh, ksh, fish)
- `SHAI_SKIP_HISTORY` environment variable to disable shell history writing
- Documented Python 3.10+ requirement for Linux installations

## v0.3.24 (2024-05-05)

### Fixed

- Handle missing `VERSION_ID` in Linux system info by falling back to `BUILD_ID`

## v0.3.23 (2024-03-08)

### Changed

- Upgraded LangChain to 0.1.11 with new langchain-openai package for improved OpenAI integration
- Upgraded OpenAI SDK to 1.13.1 for latest API compatibility

## v0.3.22 (2024-01-06)

### Added

- GitHub Actions workflow for automated PyPI publishing
- `OPENAI_MAX_TOKENS` environment variable to control maximum tokens in API responses

### Changed

- Updated Shell-AI tagline to "let AI write your shell commands"

## v0.3.21 (2023-12-16)

### Added

- `--ctx` command-line flag to enable context mode without relying on environment variables

## v0.3.20 (2023-12-10)

### Added

- `CTX` environment variable to use console outputs as context for improved LLM suggestions
- "Enter a new command" option to re-prompt within the same session without regenerating suggestions
- Platform information (OS, distribution, version) in system message for better OS-specific command generation

### Changed

- Command execution now captures and displays output in context mode for iterative improvement

## v0.3.18 (2023-09-24)

### Fixed

- Duplicate commands are now deduplicated before displaying suggestions

## v0.3.17 (2023-09-14)

### Fixed

- Graceful handling for Ctrl+C (KeyboardInterrupt) during command selection menu

## v0.3.16 (2023-09-06)

### Added

- Markdown code block parsing to handle AI responses with Markdown-formatted code blocks

### Changed

- AI system prompt format now expects JSON responses wrapped in Markdown code blocks
- JSON parsing error handling now falls back to treating response as command instead of printing error message

## v0.3.14 (2023-09-06)

### Fixed

- Command execution failing when user confirmation prompt is skipped

## v0.3.13 (2023-09-06)

### Added

- `SHAI_SKIP_CONFIRM` environment variable to skip command execution confirmation

## v0.3.12 (2023-09-06)

### Added

- Command confirmation prompt allowing users to review and edit suggested commands before execution

## v0.3.11 (2023-08-23)

### Fixed

- Azure API base environment variable check now only required when using Azure provider

## v0.3.10 (2023-08-23)

### Added

- Azure OpenAI API support via `OPENAI_API_TYPE` environment variable (set to `azure`)
- New environment variables for Azure deployments: `OPENAI_API_VERSION`, `AZURE_DEPLOYMENT_NAME`, `AZURE_API_BASE`

## v0.3.9 (2023-08-21)

### Added

- `OPENAI_ORGANIZATION` environment variable for OpenAI Organization ID configuration
- `OPENAI_PROXY` environment variable for OpenAI proxy configuration

## v0.3.8 (2023-08-21)

### Added

- `OPENAI_API_BASE` environment variable to specify custom API endpoint or proxy service

### Changed

- Configuration file security guidance now includes instructions to restrict permissions with `chmod 600` on Linux/macOS

## v0.3.7 (2023-08-21)

### Fixed

- Configuration values properly converted to strings when setting environment variables

## v0.3.6 (2023-08-21)

### Changed

- Shell command suggestions now wrap to terminal width for better readability

## v0.3.5 (2023-08-21)

### Added

- Error message informing users about config.json alternative for API key configuration

## v0.3.4 (2023-08-21)

### Fixed

- "Generate new suggestions" option now works correctly (was checking for incorrect string)

## v0.3.3 (2023-08-21)

### Changed

- Updated option text from "Generate a new suggestion" to "Generate new suggestions" in interactive prompt

## v0.3.2 (2023-08-21)

### Changed

- Renamed environment variable `AIS_SUGGESTION_COUNT` to `SHAI_SUGGESTION_COUNT`

## v0.3.1 (2023-08-21)

### Changed

- Package metadata now includes README.md content for improved PyPI package description

## v0.3.0 (2023-08-21)

### Added

- Configuration file support for Linux/macOS (`~/.config/shell-ai/config.json`) and Windows (`%APPDATA%\shell-ai\config.json`)
- `OPENAI_MODEL` environment variable (defaults to `gpt-3.5-turbo`)
- `AIS_SUGGESTION_COUNT` environment variable (defaults to 3)

### Changed

- CLI command name changed from `ais` to `shai`
- Package name changed from `ai-shell` to `shell-ai`
- Project branding updated from "AI-Shell" to "Shell-AI" throughout documentation

## v0.1.0 (2023-08-20)

### Added

- Initial release