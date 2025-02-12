# otel-worker-core

This crate contains the shared types and logic that is used in both the normal
otel-worker-cli binary and the otel-worker crates. This crate contains most of
the logic that is shared between them and it also includes shared traits. The
implementations of these traits might not be included in this crate.
