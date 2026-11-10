//! Fork (perf watch): `herdr server perf [--json] [--reset]`, the running
//! server's frame, render and stall counters (`server.perf_summary`).

use crate::api::schema::{Method, Request, ServerPerfSummaryParams};

pub(super) const USAGE: &str = "usage: herdr server perf [--json] [--reset]";

pub(super) fn server_perf(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    let mut reset = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--reset" => reset = true,
            _ => {
                eprintln!("{USAGE}");
                return Ok(2);
            }
        }
    }
    let response = super::send_request(&request(reset))?;
    if is_unsupported(&response) {
        eprintln!(
            "this server does not report perf counters (server.perf_summary); update and restart it"
        );
        return Ok(1);
    }
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    print!("{}", format_summary(&response["result"]["perf"], reset));
    Ok(0)
}

fn request(reset: bool) -> Request {
    Request {
        id: "cli:server:perf".into(),
        method: Method::ServerPerfSummary(ServerPerfSummaryParams { reset }),
    }
}

/// A server without the method rejects the request as an unknown variant.
pub(super) fn is_unsupported(response: &serde_json::Value) -> bool {
    let Some(error) = response.get("error") else {
        return false;
    };
    let code = error.get("code").and_then(serde_json::Value::as_str);
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(
        code,
        Some("invalid_request" | "unknown_method" | "method_not_found")
    ) && message.contains(crate::api::schema::perf::METHOD)
}

fn u(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KiB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        s if s >= 3600 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{s}.{}s", (ms % 1000) / 100),
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0%".into()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

/// The human form of a `server.perf_summary` result.
pub(super) fn format_summary(perf: &serde_json::Value, reset: bool) -> String {
    let full = u(perf, "full_renders");
    let retained = u(perf, "retained_renders");
    let fallbacks = u(perf, "retained_fallbacks");
    let frames = u(perf, "frames_sent");
    let avg_us = u(perf, "full_render_total_us")
        .checked_div(full)
        .unwrap_or(0);
    let mut out = format!(
        "server pid {} · counters cover {} · generation {}\n",
        u(perf, "pid"),
        duration_ms(u(perf, "window_ms")),
        u(perf, "generation"),
    );
    out.push_str(&format!(
        "frames sent  {frames} ({}), {} deferred by a full client channel\n",
        bytes(u(perf, "frame_bytes")),
        u(perf, "frames_deferred"),
    ));
    out.push_str(&format!(
        "full renders {full}, avg {:.1} ms, max {:.1} ms\n",
        avg_us as f64 / 1000.0,
        u(perf, "full_render_max_us") as f64 / 1000.0,
    ));
    out.push_str(&format!(
        "retained     {retained}, fallbacks to full {fallbacks} ({})\n",
        percent(fallbacks, retained + fallbacks),
    ));
    out.push_str(&format!(
        "sync holds   {}, released by timeout {}\n",
        u(perf, "sync_holds"),
        u(perf, "sync_hold_timeouts"),
    ));
    for lp in perf
        .get("loops")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let busy = u(lp, "busy_ms");
        out.push_str(&format!(
            "loop {}: {} stalls over {} ms, longest {} ms{}\n",
            lp.get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            u(lp, "stalls"),
            u(lp, "threshold_ms"),
            u(lp, "longest_stall_ms"),
            if busy > 0 {
                format!(", busy now {busy} ms")
            } else {
                String::new()
            },
        ));
    }
    for client in perf
        .get("clients")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = client
            .get("client_id")
            .and_then(serde_json::Value::as_u64)
            .map_or_else(|| "others".to_owned(), |id| format!("client {id}"));
        out.push_str(&format!(
            "{label}: {} frames, {}\n",
            u(client, "frames"),
            bytes(u(client, "bytes")),
        ));
    }
    if reset {
        out.push_str("counters reset\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_smoke_perf_an_old_server_without_the_method_reads_as_not_supported() {
        // What a server that predates server.perf_summary answers: its Request
        // type has no such variant, so the API front door (src/api/server.rs)
        // rejects the line as `invalid request: {serde error}`. Take the real
        // serde message for a method this build lacks and put the name back.
        let line = serde_json::to_string(&request(false))
            .expect("request json")
            .replace("server.perf_summary", "server.zz_missing");
        let serde_error = serde_json::from_str::<Request>(&line).expect_err("unknown method");
        let old_server_error = format!("invalid request: {serde_error}")
            .replace("server.zz_missing", "server.perf_summary");
        assert!(
            old_server_error.contains("unknown variant"),
            "{old_server_error}"
        );
        let response = serde_json::json!({
            "id": "cli:server:perf",
            "error": {"code": "invalid_request", "message": old_server_error},
        });
        assert!(is_unsupported(&response));

        // Other errors stay errors.
        let other = serde_json::json!({
            "id": "cli:server:perf",
            "error": {"code": "server_unavailable", "message": "server is shutting down"},
        });
        assert!(!is_unsupported(&other));
        let success = serde_json::json!({"id": "x", "result": {"type": "server_perf_summary"}});
        assert!(!is_unsupported(&success));
    }

    #[test]
    fn fork_smoke_perf_the_request_omits_reset_unless_asked() {
        let plain = serde_json::to_value(request(false)).expect("json");
        assert_eq!(
            plain,
            serde_json::json!({"id": "cli:server:perf", "method": "server.perf_summary", "params": {}})
        );
        let reset = serde_json::to_value(request(true)).expect("json");
        assert_eq!(reset["params"], serde_json::json!({"reset": true}));
    }

    #[test]
    fn fork_smoke_perf_the_summary_reads_as_lines() {
        let perf = serde_json::json!({
            "pid": 4242, "started_unix_ms": 1, "generation": 0, "window_ms": 754_000,
            "frames_sent": 12034, "frame_bytes": 3_355_443, "frames_deferred": 2,
            "full_renders": 512, "full_render_total_us": 2_150_400, "full_render_max_us": 9_300,
            "retained_renders": 11_000, "retained_fallbacks": 30,
            "sync_holds": 800, "sync_hold_timeouts": 2,
            "loops": [{"name": "server_loop", "threshold_ms": 250, "stalls": 1,
                       "longest_stall_ms": 412, "busy_ms": 0}],
            "clients": [{"client_id": 3, "frames": 12034, "bytes": 3_355_443}],
        });
        assert_eq!(
            format_summary(&perf, false),
            "server pid 4242 · counters cover 12m34s · generation 0\n\
             frames sent  12034 (3.2 MiB), 2 deferred by a full client channel\n\
             full renders 512, avg 4.2 ms, max 9.3 ms\n\
             retained     11000, fallbacks to full 30 (0.3%)\n\
             sync holds   800, released by timeout 2\n\
             loop server_loop: 1 stalls over 250 ms, longest 412 ms\n\
             client 3: 12034 frames, 3.2 MiB\n"
        );
    }
}
