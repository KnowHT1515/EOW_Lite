# EOW_Lite

`EOW_Lite` is a lightweight, standalone runtime that implements selected features of `Echoes of the White`. Written in Rust, it provides a Windows terminal interface for conversations with a configured language model.

## Quick Start

You need Windows, `Rust` 1.96 or later with Cargo, and access to a model service that supports the Chat Completions format. Run the commands below in PowerShell from the repository root.

### 1. Build

```powershell
cargo build --workspace --bins --release --locked
```

### 2. Configure the Model

For a fresh checkout, copy the example configuration and prompt files.

```powershell
$liteConfig = '.\glimpse_of_the_white\lite\config'
Copy-Item -LiteralPath "$liteConfig\example\core.toml" -Destination "$liteConfig\core.toml"
Copy-Item -LiteralPath "$liteConfig\example\prompt" -Destination "$liteConfig\prompt" -Recurse
```

Edit the `[LLM_Model]` section in the copied `core.toml`.

```toml
[LLM_Model]
url = "https://your-provider.example/v1/chat/completions"
model = "<model-id>"
api_key = "<your-api-key>"
```

Use the full endpoint URL from your model provider. Core sends requests directly to that URL. Keep the other sections from the template for your first run.

Git ignores the local runtime configuration. Keep real credentials in the local `core.toml` and leave placeholder values in `config/example/`.

### 3. Start a Conversation

```powershell
.\eow-lite.cmd link
```

Supervisor starts Transport, Core, Adapter, and UI in that order. The control port is set by `supervisor_port` in [lifecycle.json](glimpse_of_the_white/lite/eow_lite/lifecycle.json). With the default configuration, Supervisor listens on `127.0.0.1:7799`.

The UI opens in a new Windows Terminal window and falls back to a system console if Windows Terminal cannot be launched. Once Supervisor reports `EOW Lite Online`, type a message in the UI and press Enter.

User messages have a white `>` prefix. Model replies have a yellow `<` prefix and appear progressively. Use the `Up` and `Down` arrow keys or `Page Up` and `Page Down` to scroll through the conversation.

### 4. Shut Down

From a second PowerShell window in the repository root, run the shutdown command.

```powershell
.\eow-lite.cmd unlink
```

You can also press `Ctrl+C` in the terminal running Supervisor. Modules stop in reverse startup order. If you add the repository root to `PATH`, you can use `eow-lite link` and `eow-lite unlink` directly.

## Modules

| Module | Responsibility |
| --- | --- |
| Transport | Routes requests and responses over local TCP and delivers events to topic subscribers. |
| Core | Assembles prompts, calls the model endpoint, splits replies, and publishes conversation events. |
| Adapter | Currently participates in module startup and shutdown. |
| UI | Sends terminal input to Core through Transport and renders conversation events. |

[module.json](glimpse_of_the_white/lite/eow_lite/module.json) defines the module registry, [ports.json](glimpse_of_the_white/lite/eow_lite/ports.json) assigns module port numbers, and [lifecycle.json](glimpse_of_the_white/lite/eow_lite/lifecycle.json) defines the module lifecycle.

See the protocol references for details:
- [Transport](glimpse_of_the_white/lite/docs/Transport.md)
- [Core](glimpse_of_the_white/lite/docs/Core.md)
- [Adapter](glimpse_of_the_white/lite/docs/Adapter.md)
- [UI](glimpse_of_the_white/lite/docs/UI.md)