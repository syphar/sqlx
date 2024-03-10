use crate::connection::LogSettings;
use pin_project::{pin_project, pinned_drop};
use std::{pin::Pin, time::Instant};

// Yes these look silly. `tracing` doesn't currently support dynamic levels
// https://github.com/tokio-rs/tracing/issues/372
#[doc(hidden)]
#[macro_export]
macro_rules! private_tracing_dynamic_enabled {
    (target: $target:expr, $level:expr) => {{
        use ::tracing::Level;

        match $level {
            Level::ERROR => ::tracing::enabled!(target: $target, Level::ERROR),
            Level::WARN => ::tracing::enabled!(target: $target, Level::WARN),
            Level::INFO => ::tracing::enabled!(target: $target, Level::INFO),
            Level::DEBUG => ::tracing::enabled!(target: $target, Level::DEBUG),
            Level::TRACE => ::tracing::enabled!(target: $target, Level::TRACE),
        }
    }};
    ($level:expr) => {{
        $crate::private_tracing_dynamic_enabled!(target: module_path!(), $level)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! private_tracing_dynamic_span {
    (target: $target:expr, $level:expr, $name:expr, $($args:tt)*) => {{
        use ::tracing::Level;

        match $level {
            Level::ERROR => ::tracing::span!(target: $target, Level::ERROR, $name, $($args)*),
            Level::WARN => ::tracing::span!(target: $target, Level::WARN, $name, $($args)*),
            Level::INFO => ::tracing::span!(target: $target, Level::INFO, $name, $($args)*),
            Level::DEBUG => ::tracing::span!(target: $target, Level::DEBUG, $name, $($args)*),
            Level::TRACE => ::tracing::span!(target: $target, Level::TRACE, $name, $($args)*),
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! private_tracing_dynamic_event {
    (target: $target:expr, $level:expr, $($field:tt)*) => {{
        use ::tracing::Level;

        match $level {
            Level::ERROR => ::tracing::event!(target: $target, Level::ERROR, $($field)*),
            Level::WARN => ::tracing::event!(target: $target, Level::WARN, $($field)*),
            Level::INFO => ::tracing::event!(target: $target, Level::INFO, $($field)*),
            Level::DEBUG => ::tracing::event!(target: $target, Level::DEBUG, $($field)*),
            Level::TRACE => ::tracing::event!(target: $target, Level::TRACE, $($field)*),
        }
    }};
}

#[doc(hidden)]
pub fn private_level_filter_to_levels(
    filter: log::LevelFilter,
) -> Option<(tracing::Level, log::Level)> {
    let tracing_level = match filter {
        log::LevelFilter::Error => Some(tracing::Level::ERROR),
        log::LevelFilter::Warn => Some(tracing::Level::WARN),
        log::LevelFilter::Info => Some(tracing::Level::INFO),
        log::LevelFilter::Debug => Some(tracing::Level::DEBUG),
        log::LevelFilter::Trace => Some(tracing::Level::TRACE),
        log::LevelFilter::Off => None,
    };

    tracing_level.zip(filter.to_level())
}

pub use sqlformat;

static QUERY_SPAN: &str = "db.query";

#[pin_project(PinnedDrop)]
pub struct QueryLogger<'q> {
    sql: &'q str,
    rows_returned: u64,
    rows_affected: u64,
    start: Instant,
    settings: LogSettings,
    #[pin]
    span: Option<tracing::span::EnteredSpan>,
}

impl<'q> QueryLogger<'q> {
    pub fn new(sql: &'q str, settings: LogSettings) -> Self {
        let span = if let Some((tracing_level, _)) =
            private_level_filter_to_levels(settings.statements_trace_level)
        {
            if private_tracing_dynamic_enabled!(target: "sqlx::query", tracing_level) {
                let span = private_tracing_dynamic_span!(target: "sqlx::query", tracing_level, QUERY_SPAN, message = sql);
                Some(span.entered())
            } else {
                None
            }
        } else {
            None
        };

        Self {
            sql,
            rows_returned: 0,
            rows_affected: 0,
            start: Instant::now(),
            settings,
            span,
        }
    }

    pub fn increment_rows_returned(&mut self) {
        self.rows_returned += 1;
    }

    pub fn increase_rows_affected(&mut self, n: u64) {
        self.rows_affected += n;
    }

    pub fn finish(&mut self) {
        if let Some(guard) = self.span.take() {
            drop(guard);
        }

        let elapsed = self.start.elapsed();

        let was_slow = elapsed >= self.settings.slow_statements_duration;

        let lvl = if was_slow {
            self.settings.slow_statements_level
        } else {
            self.settings.statements_level
        };

        if let Some((tracing_level, log_level)) = private_level_filter_to_levels(lvl) {
            // The enabled level could be set from either tracing world or log world, so check both
            // to see if logging should be enabled for our level
            let log_is_enabled = log::log_enabled!(target: "sqlx::query", log_level)
                || private_tracing_dynamic_enabled!(target: "sqlx::query", tracing_level);
            if log_is_enabled {
                let mut summary = parse_query_summary(&self.sql);

                let sql = if summary != self.sql {
                    summary.push_str(" …");
                    format!(
                        "\n\n{}\n",
                        sqlformat::format(
                            &self.sql,
                            &sqlformat::QueryParams::None,
                            sqlformat::FormatOptions::default()
                        )
                    )
                } else {
                    String::new()
                };

                if was_slow {
                    private_tracing_dynamic_event!(
                        target: "sqlx::query",
                        tracing_level,
                        summary,
                        db.statement = sql,
                        rows_affected = self.rows_affected,
                        rows_returned = self.rows_returned,
                        // Human-friendly - includes units (usually ms). Also kept for backward compatibility
                        ?elapsed,
                        // Search friendly - numeric
                        elapsed_secs = elapsed.as_secs_f64(),
                        // When logging to JSON, one can trigger alerts from the presence of this field.
                        slow_threshold=?self.settings.slow_statements_duration,
                        // Make sure to use "slow" in the message as that's likely
                        // what people will grep for.
                        "slow statement: execution time exceeded alert threshold"
                    );
                } else {
                    private_tracing_dynamic_event!(
                        target: "sqlx::query",
                        tracing_level,
                        summary,
                        db.statement = sql,
                        rows_affected = self.rows_affected,
                        rows_returned = self.rows_returned,
                        // Human-friendly - includes units (usually ms). Also kept for backward compatibility
                        ?elapsed,
                        // Search friendly - numeric
                        elapsed_secs = elapsed.as_secs_f64(),
                    );
                }
            }
        }
    }
}

#[pinned_drop]
impl PinnedDrop for QueryLogger<'_> {
    fn drop(mut self: Pin<&mut Self>) {
        self.finish();
    }
}

pub fn parse_query_summary(sql: &str) -> String {
    // For now, just take the first 4 words
    sql.split_whitespace()
        .take(4)
        .collect::<Vec<&str>>()
        .join(" ")
}
