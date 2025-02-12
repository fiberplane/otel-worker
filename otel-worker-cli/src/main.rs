use anyhow::{Context, Result};
use clap::Parser;
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{runtime, Resource};
use std::env;
use tracing::trace;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

mod commands;
pub mod data;
pub mod events;
pub mod grpc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = commands::Args::parse();
    let should_shutdown_tracing = args.enable_tracing;

    setup_tracing(&args)?;

    let result = commands::handle_command(args).await;

    if should_shutdown_tracing {
        trace!("Shutting down tracers");
        shutdown_tracing();
    }

    result
}

/// Log directives that uses info for all otel_worker related crates and error
/// for everything else.
const DEFAULT_LOG_DIRECTIVES: &str = "otel_worker=info,error";

/// Log directives that uses debug for all otel_worker related crates and info
/// for everything else.
const DEBUG_LOG_DIRECTIVES: &str = "otel_worker=debug,info";

fn setup_tracing(args: &commands::Args) -> Result<()> {
    let filter_layer = {
        let rust_log = env::var("RUST_LOG");
        let directives = rust_log.as_deref().unwrap_or_else(|_| {
            if args.debug {
                DEBUG_LOG_DIRECTIVES
            } else {
                DEFAULT_LOG_DIRECTIVES
            }
        });

        EnvFilter::builder().parse(directives)?
    };

    let log_layer = tracing_subscriber::fmt::layer();

    let trace_layer = if args.enable_tracing {
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(args.otlp_endpoint.to_string())
            .build()?;

        // This tracer is responsible for sending the actual traces.
        let tracer_provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "otel-worker-cli",
            )]))
            .with_batch_exporter(exporter, runtime::Tokio)
            .build();

        opentelemetry::global::set_tracer_provider(tracer_provider.clone());

        let tracer = tracer_provider.tracer("otel-worker-cli");

        // This layer will take the traces from the `tracing` crate and send
        // them to the tracer specified above.
        Some(OpenTelemetryLayer::new(tracer))
    } else {
        None
    };

    Registry::default()
        .with(filter_layer)
        .with(log_layer)
        .with(trace_layer)
        .try_init()
        .context("unable to initialize logger")?;

    Ok(())
}

fn shutdown_tracing() {
    opentelemetry::global::shutdown_tracer_provider();
}
