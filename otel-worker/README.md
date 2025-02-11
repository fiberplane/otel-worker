# otel-worker

The `otel-worker` crate is an implementation of the [OTLP/HTTP spec][otlphttp].
Currently both JSON and Protocol Buffers encoding are supported. gRPC is not
supported for the `otel-worker`.

This crate also includes http endpoints to retrieve the traces and a web-socket
endpoint to receive realtime notification about newly added traces.

## Authentication

The `otel-worker` allows for a simple bearer token authentication. This token is
required by the OTLP/HTTP endpoints and the traces endpoints.

## Local development

To get started you will need to make sure that you have Rust and wrangler
installed. See their respective documentation for installation instructions.

Using the wrangler CLI you need to create a new D1 database to use and then
apply all the migrations on it:

```
npx wrangler d1 create DB
npx wrangler d1 migrations apply DB
```

You only need to create the database once, but you might have to run the
migrations for new versions of the `otel-worker`.

You can now run `otel-worker` using the wrangler CLI:

```
npx wrangler dev
```

The Rust code will be compiled and once that is finished a local server will be
running on `http://localhost:8787`. You can send traces using any OTLP/HTTP
compatible exporter and inspect the traces using the [`client`](../fpx-cli).

## Deploying to Cloudflare

If you want to deploy this worker to Cloudflare you will require a paid account
(since this is using durable objects). You still need to go through the same
steps to create a database, but remember to add the `--remote` flag when running
the d1 commands.

After the database has been created and the migrations have been applied, you
need run the following command to compile the worker and upload it to your
Cloudflare environment:

```
npx wrangler deploy
```

Once the compilation and upload is finished, you will be informed about the URL
where the worker is running. Optionally you can use `--name` to use a different
name for the worker (if you want to run multiple instances, for different
environments).

[otlphttp]: https://opentelemetry.io/docs/specs/otlp/#otlphttp
