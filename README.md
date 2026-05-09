# my-ci

<img src="./my-ci.png" alt="my-ci" width="75%">


Run local CI/CD workflows over Docker, Podman, or Apple container on macOS.

## Demo

```sh
git clone https://github.com/geoffsee/my-ci.git
cd my-ci
docker compose up
```

Then open `http://127.0.0.1:7878`, click `Run`, and watch the pipeline execute in realtime.

## Install

```sh
cargo binstall my-ci
```

## Quickstart

```sh
my-ci init        # scaffold ./my-ci
my-ci run         # build + run the pipeline
```

## Commands

| Command | Args                                   | Description                                                              |
| ------- | -------------------------------------- | ------------------------------------------------------------------------ |
| `init`  | `[PATH]` (default `my-ci`), `--force`  | Scaffold the embedded template into `PATH`. Skips existing unless force. |
| `build` | `[WORKFLOW]`                           | Build one workflow + deps, or all workflows when omitted.                |
| `run`   | `[WORKFLOW]`                           | Build deps, then run workflows that have a `command`. All when omitted.  |
| `list`  | —                                      | Print workflow names from config.                                        |

Global:

- `-c, --config <PATH>` (default `my-ci/workflows.toml`)
- `--runtime <auto|docker|podman|apple-container>` (default `auto`)

On macOS, `auto` uses the `container` CLI when it is installed and its service is running. Otherwise it falls back to a Docker socket, then a Podman socket. To force Apple's runtime, run:

```sh
my-ci --runtime apple-container run
```

Apple container requires the `container` CLI and its system service. Start it with:

```sh
container system start
```

The GUI exposes the same runtime choices for build/run requests. The Apple container option is shown only when the browser reports a macOS platform.

## Debugging

`my-ci` emits structured traces to stderr. The default filter enables app-level info traces. Override it with `RUST_LOG` when you need more or less detail:

```sh
RUST_LOG=my_ci=trace my-ci run
RUST_LOG=my_ci=debug,bollard=warn my-ci --runtime docker build
```

## `workflows.toml` schema

```toml
name      = "string"          # project name; used as image prefix (default "my-ci")
env_file  = "path"             # optional; loaded via dotenvy before run, relative to config

[[workflow]]
name         = "string"        # required; unique
context      = "path"          # build context; default "."
instructions = "string"        # inline Containerfile OR path ending in .Containerfile
image        = "string"        # optional override; default "{name}:{workflow.name}"
depends_on   = ["string"]      # build order; topologically sorted
env          = ["KEY=VALUE"]   # container env at run time
command      = ["argv"]        # required to run; build-only if omitted
```

Dependencies build in topological order. A workflow without `command` is build-only and is skipped by `run`.
