# Changelog

[![GitHub releases](https://img.shields.io/github/release/Deltik/shell-ai.svg)](https://github.com/Deltik/shell-ai/releases)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.5.2 (UNRELEASED)

### Added

- **Multi-line command support for Ctrl+G keybinding**

  The keybinding integrations for all shells (Bash, Zsh, Fish, PowerShell) now support multi-line command output, in case the generated suggestion spans multiple lines. Previously, only the first line was used.

### Fixed

- **Bash keybinding integration: first argument no longer ignored**

  Fixed a bug where the Bash keybinding (Ctrl+G) would corrupt the command line, causing the first word to be lost. For example, `uptime -s` would display correctly but execute as two separate commands (`uptime` then `-s`).

  The root cause is a Bash bug where `$(< file)` corrupts `READLINE_LINE` when used in a `bind -x` context. The workaround uses `$(cat file)` instead, which forks a subprocess and avoids the corruption.

  This bug is not present in Bash 5.3.

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