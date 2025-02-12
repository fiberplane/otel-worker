# otel-worker-cli

The `otel-worker-cli` binary is a command line tool to do certain tasks from the
command line which apply to the `otel-worker`. It can be used to spawn a local
dev server (so without any integration with wrangler/Cloudflare), it can also be
used as a client to interact with the `otel-worker` server.

## Usage

First, make sure you have Rust installed on your machine. You can install Rust
using [rustup](https://rustup.rs/) or take a look at the
[official instructions](https://www.rust-lang.org/tools/install).

Then run the following command to execute the local dev server:

```
cargo run -- dev
```

## Commands

This section highlights some of the commands that can be used. For a full list
use the `--help` flag when invoking the `otel-worker-cli` binary.

For ease of use, the `cli` cargo alias has been added, meaning you can run
`cargo cli` in any directory in this repository, which will then compile and
invoke the `otel-worker-cli` binary. Any extra arguments are passed to the
`otel-worker-cli` binary

### `otel-worker-cli dev`

Starts the local dev server. This is a local version of the `otel-worker`.

Use `-e` or `--enable-tracing` to send otlp payloads to `--otlp-endpoint`. Note
that this should not be used to send traces to itself, as that will create an
infinite loop.

### `otel-worker-cli client`

Invokes endpoints on a `otel-worker` server.

This command can also send traces to a otel endpoint. NOTE: there are some known
issues where it doesn't seem to work properly.

Examples:

```
# Fetch all traces
otel-worker-cli client traces list

# Fetch a specific span
otel-worker-cli client spans get aa aa
```
