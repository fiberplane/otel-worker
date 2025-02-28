# otel-worker

This is the main implementation of the otel-worker. It is a Cloudflare Worker
that is responsible for receiving OpenTelemetry traces and storing them in a
D1 database.

See the main [README.md](../README.md) for more information about how to run the
worker.

## Sending traces to the worker

The worker accepts traces via OTLP/HTTP. The endpoint to send traces to is
`/v1/traces`.

Here's an example of how to send traces to the worker using `curl` from within
the `examples/send-trace` directory:

```sh
curl -X POST http://localhost:24318/v1/traces \
  -H "Authorization: Bearer your-secret-token-here" \
  -H "Content-Type: application/json" \
  --data-binary @trace.json
```

## Getting traces from the worker

The worker exposes a `/v1/traces` endpoint that can be used to retrieve traces from
the database.

Here's an example of how to retrieve traces from the worker using `curl`:

```sh
curl http://localhost:8787/v1/traces \
  -H "Authorization: Bearer your-secret-token-here"
```

Make sure to update the `your-secret-token-here` with the token you set in the
`.dev.vars` file. An auth token is required to access the `/v1/traces` endpoint.
