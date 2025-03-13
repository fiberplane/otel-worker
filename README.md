This repository contains a collection of Rust related projects for OpenTelemtry and Cloudflare Workers.

## otel-worker

An OpenTelemtry (OTEL) collector that is intended to be run on Clouflare Workers and uses D1 to
store the traces. It also has a few endpoints to expose these traces.

The repository is organized as follows:

```
📦 otel-worker (root)
├── 📂 otel-worker/                # Main Cloudflare Worker implementation
│   ├── 📂 src/                    # Worker-specific source code
│   ├── 📂 examples/               # Example implementations and usage demos
│   └── 📂 migrations/             # D1 database schema migrations
│
├── 📂 otel-worker-core/           # Core business logic library
│   └── 📂 src/                    # Platform-agnostic OpenTelemetry implementation
│
├── 📂 otel-worker-macros/         # Custom Rust procedural macros
│   └── 📂 src/                    # Code generation and derive macros
│
├── 📂 otel-worker-cli/            # Command-line interface tools
│   └── 📂 src/                    # CLI utilities for worker interaction
│
├── 📂 xtask/                      # Build automation and development tasks
│   └── 📂 src/                    # Custom build scripts and workflows
```

The [otel-worker/README.md](otel-worker/) folder contains the main collector implementation discussed below.

# otel-worker

The `otel-worker` crate is an implementation of the [OTLP/HTTP spec][otlphttp].
Currently both JSON and Protocol Buffers encoding are supported. gRPC is not
supported for the `otel-worker`.

This crate also includes http endpoints to retrieve the traces and a web-socket
endpoint to receive realtime notification about newly added traces.

## Authentication

The `otel-worker` allows for a simple bearer token authentication. This token is
*required* by the OTLP/HTTP endpoints and the traces endpoints. 

You can configure the token using the `AUTH_TOKEN` environment variable (see below).

## Local development

To get started you will need to make sure that you have Rust and wrangler
installed. See their respective documentation for installation instructions.

There are three steps to set up the database:

1. Create the database
2. Update the wrangler.toml file to include the database name and id
3. Apply the migrations

Create a new D1 database using the wrangler CLI:

```sh
npx wrangler d1 create fiberplane-otel-db
```

The output of the command will include the database id, which you will need to update in the `wrangler.toml` file.

Update your `wrangler.toml` as follows:

```toml
database_name = "fiberplane-otel-db"
# change the databse_id to whatever was output by the wrangler d1 create command
database_id = "id-of-fiberplane-otel-db"
```

Apply the migrations:

```sh
npx wrangler d1 migrations apply fiberplane-otel-db
```

You only need to create the database once, but you might have to run the
migrations for new versions of the `otel-worker`.

As a final step, copy the file `otel-worker/.dev.vars.example` to `otel-worker/.dev.vars` and set the `AUTH_TOKEN` to a value of your choice. For local development, you can keep the default value:

```sh
AUTH_TOKEN="your-secret-token-here"
```

This configures the otel worker with a secret token that is used to authenticate
requests to the api endpoints.

You can now run `otel-worker` using the wrangler CLI:

> **Note**: Compiling the Worker is not supported on Windows at the moment
> without WSL.

```sh
npx wrangler dev
```

The Rust code will be compiled and once that is finished a local server will be
running on `http://localhost:24318`. You can send traces using any OTLP/HTTP
compatible exporter and inspect the traces using the
[`client`](../otel-worker-cli).

## Deploying to Cloudflare

If you want to deploy this worker to Cloudflare you will require a paid account
(since this worker uses Durable Objects). You still need to go through the same
steps to create a database, but remember to add the `--remote` flag when running
the D1 commands.

```sh
# Migrate production database
npx wrangler d1 migrations apply fiberplane-otel-db --remote
```

After the database has been created and the migrations have been applied, you
need run the following command to compile the worker and upload it to your
Cloudflare environment:

```sh
npx wrangler deploy
```

Once the compilation and upload is finished, you will be informed about the URL
where the worker is running. Optionally you can use `--name` to use a different
name for the worker (if you want to run multiple instances, for different
environments).

As a final step, you need to set an `AUTH_TOKEN` secret on your Worker:

```sh
npx wrangler secret put AUTH_TOKEN
```

[otlphttp]: https://opentelemetry.io/docs/specs/otlp/#otlphttp

