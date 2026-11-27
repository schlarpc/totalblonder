//! Wide events and OpenTelemetry export.
//!
//! A *wide event* is one record per unit of work (an RTMP session, a decoder worker's life, a
//! source switch, a carrier's last ten seconds) that carries every field describing that work,
//! plus the fields of the context it ran in. One query over such records answers questions
//! that scattered log lines cannot, because nothing has to be joined back together.
//!
//! ```
//! use tb_telemetry::wide;
//!
//! // Context: every event and wide event on this thread inside it carries `carrier = 1`.
//! let _carrier = tracing::info_span!("carrier", carrier = 1).entered();
//!
//! let ev = wide!("input.session", input = "cam1", peer = "10.0.0.7:51234");
//! ev.add("bytes", 1316u64);
//! ev.add("bytes", 1316u64);
//! ev.set("close", "eof");
//! drop(ev); // reported here: input.session carrier=1 input=cam1 … bytes=2632 duration_ms=…
//! ```
//!
//! A [`Wide`] is a `tracing` span underneath, so it nests: ordinary log lines made while it is
//! entered carry its fields, and a wide event made inside another reports the outer one's
//! fields too. The [`layer::WideLayer`] does the reporting; [`init`] installs it with a stderr
//! formatter and, with the `otlp` feature, the OpenTelemetry exporters.
//!
//! Without a subscriber (library tests, other binaries) a wide event costs one disabled span and
//! reports nothing.

mod config;
pub mod layer;
#[cfg(feature = "otlp")]
mod otlp;
mod value;

use std::borrow::Cow;

pub use config::{Config, Error, Telemetry, init};
pub use value::Val;

/// The OpenTelemetry API this crate exports through (for registering metrics on
/// [`Telemetry::meter`]).
#[cfg(feature = "otlp")]
pub use opentelemetry;

/// The `tracing` target of every wide event's span.
pub const TARGET: &str = "wide";

/// A field name.
pub type Key = Cow<'static, str>;

/// One unit of work, reported once when the last handle to it is dropped, with every field set
/// on it, the fields of the spans it was made in, and how long it lasted (`duration_ms`).
#[must_use = "a wide event is reported when it is dropped"]
#[derive(Debug, Clone)]
pub struct Wide {
    span: tracing::Span,
}

impl Wide {
    /// Starts a wide event named `name` (dotted, `noun.verb` or `noun.interval`), inside the
    /// current span.
    pub fn new(name: &'static str) -> Wide {
        Wide {
            span: tracing::info_span!(target: TARGET, "wide", event.name = name),
        }
    }

    /// Starts a wide event inside `parent` rather than the current span: for work that a
    /// thread does on behalf of a context made on another.
    pub fn child_of(parent: &tracing::Span, name: &'static str) -> Wide {
        Wide {
            span: tracing::info_span!(target: TARGET, parent: parent, "wide", event.name = name),
        }
    }

    /// Sets a field, replacing any earlier value.
    pub fn set(&self, key: impl Into<Key>, value: impl Into<Val>) -> &Self {
        annotate(&self.span, key, value);
        self
    }

    /// [`Wide::set`], by value: for building one up in an expression.
    pub fn with(self, key: impl Into<Key>, value: impl Into<Val>) -> Self {
        annotate(&self.span, key, value);
        self
    }

    /// Adds to a numeric field (absent counts as zero).
    pub fn add(&self, key: impl Into<Key>, n: impl Into<Val>) -> &Self {
        layer::with_fields(&self.span, |f| f.add(key.into(), n.into()));
        self
    }

    /// Marks the work failed: `outcome = "error"`, `error = <e>`, reported at ERROR.
    pub fn error(&self, e: impl std::fmt::Display) -> &Self {
        self.set("outcome", "error").set("error", e.to_string());
        self.level(tracing::Level::ERROR)
    }

    /// Sets `outcome`. A failure that is not an error (a timeout, a refusal) is reported at WARN
    /// with [`Wide::level`].
    pub fn outcome(&self, outcome: &'static str) -> &Self {
        self.set("outcome", outcome)
    }

    /// The severity it is reported at (INFO unless set).
    pub fn level(&self, level: tracing::Level) -> &Self {
        layer::with_fields(&self.span, |f| f.level = Some(level));
        self
    }

    /// Reports it now, without `duration_ms`: for something that happened rather than something
    /// that took time (a switch, a step).
    pub fn emit(self) {
        layer::with_fields(&self.span, |f| f.instant = true);
    }

    /// Makes it the current span until the guard is dropped: log lines and wide events made
    /// meanwhile carry its fields.
    pub fn enter(&self) -> tracing::span::Entered<'_> {
        self.span.enter()
    }

    /// The span underneath, to be a parent elsewhere.
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }
}

/// Sets a field on any span: on a context span, every event under it gains the field from then
/// on.
pub fn annotate(span: &tracing::Span, key: impl Into<Key>, value: impl Into<Val>) {
    layer::with_fields(span, |f| f.set(key.into(), value.into()));
}

/// Starts a [`Wide`] event with fields: `wide!("name", key = value, …)`. Values are anything
/// that converts into a [`Val`]; dotted keys are written as string literals
/// (`"rtmp.app" = app`).
#[macro_export]
macro_rules! wide {
    ($name:expr $(,)?) => { $crate::Wide::new($name) };
    ($name:expr, $($k:tt = $v:expr),+ $(,)?) => {
        $crate::Wide::new($name)$(.with($crate::__key!($k), $v))+
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __key {
    ($k:ident) => {
        stringify!($k)
    };
    ($k:literal) => {
        $k
    };
}
