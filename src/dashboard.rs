use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{db::Database, AppState};

#[derive(Serialize)]
pub struct ApiStats {
    total_reviews: i64,
    approved: i64,
    request_changes: i64,
    commented: i64,
    avg_inline_comments: f64,
    critical_count: i64,
    warning_count: i64,
    info_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub repo: Option<String>,
    pub verdict: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub severity: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// Knight A (the Blackshield Company mascot) SVG for a dashboard state.
pub fn knight_svg(state: &str) -> Option<&'static str> {
    match state {
        "idle" => Some(include_str!("../static/knight/knight-idle.svg")),
        "thinking" => Some(include_str!("../static/knight/knight-thinking.svg")),
        "error" => Some(include_str!("../static/knight/knight-error.svg")),
        _ => None,
    }
}

/// Serves the knight used by the dashboard's empty, loading and error states.
pub async fn knight_handler(Path(state): Path<String>) -> Response {
    match knight_svg(&state) {
        Some(svg) => (
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            svg,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn dashboard_handler(State(state): State<AppState>) -> Html<String> {
    let db = state.database.clone();
    let refresh = state.config.dashboard_config().refresh_seconds;

    match render_dashboard(&db, refresh).await {
        Ok(html) => Html(html),
        Err(e) => Html(format!(
            "<body style=\"background:#0d1117;color:#c9d1d9;font-family:sans-serif;text-align:center;padding:4rem\">\
             <img src=\"/dashboard/knight/error\" width=\"128\" height=\"112\" alt=\"\" style=\"image-rendering:pixelated\">\
             <h1>Dashboard Error</h1><p>{}</p></body>",
            escape_html(&e.to_string())
        )),
    }
}

pub async fn stats_api_handler(State(state): State<AppState>) -> Json<ApiStats> {
    let db = state.database.clone();

    match db.get_stats().await {
        Ok(stats) => Json(ApiStats {
            total_reviews: stats.total_reviews,
            approved: stats.approved,
            request_changes: stats.request_changes,
            commented: stats.commented,
            avg_inline_comments: stats.avg_inline_comments,
            critical_count: stats.critical_count,
            warning_count: stats.warning_count,
            info_count: stats.info_count,
        }),
        Err(_) => Json(ApiStats {
            total_reviews: 0,
            approved: 0,
            request_changes: 0,
            commented: 0,
            avg_inline_comments: 0.0,
            critical_count: 0,
            warning_count: 0,
            info_count: 0,
        }),
    }
}

pub async fn search_api_handler(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<crate::db::ReviewRecord>> {
    let db = state.database.clone();

    let filters = crate::db::ReviewSearchFilters {
        repo: query.repo,
        verdict: query.verdict,
        from: query.from.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        to: query.to.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        severity: query.severity,
        limit: query.limit,
    };

    match db.search_reviews(&filters).await {
        Ok(results) => Json(results),
        Err(_) => Json(vec![]),
    }
}

async fn render_dashboard(db: &Database, refresh_seconds: u64) -> anyhow::Result<String> {
    let stats = db.get_stats().await?;
    let recent = db.get_recent_reviews(50).await?;

    let mut rows = String::new();
    for review in &recent {
        let emoji = match review.verdict.as_str() {
            "Approve" => "✅",
            "RequestChanges" => "❌",
            _ => "💬",
        };
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}#{}</td><td>{} {}</td><td>{}</td><td>{}</td></tr>\n",
            review.created_at.format("%Y-%m-%d %H:%M"),
            review.provider,
            review.repo,
            review.pr_number,
            emoji,
            review.verdict,
            review.inline_count,
            escape_html(&review.summary)
                .chars()
                .take(100)
                .collect::<String>()
        ));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>SentryClaw Dashboard</title>
    <style>
        :root {{ --bg: #0d1117; --card: #161b22; --border: #30363d; --text: #c9d1d9; --accent: #58a6ff; --success: #3fb950; --danger: #f85149; --warn: #d29922; }}
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; background: var(--bg); color: var(--text); line-height: 1.6; }}
        .container {{ max-width: 1200px; margin: 0 auto; padding: 2rem; }}
        header {{ margin-bottom: 2rem; }}
        header h1 {{ font-size: 2rem; display: flex; align-items: center; gap: 0.5rem; }}
        .stats {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; margin-bottom: 2rem; }}
        .stat-card {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; }}
        .stat-card h3 {{ font-size: 0.875rem; text-transform: uppercase; letter-spacing: 0.05em; color: #8b949e; margin-bottom: 0.5rem; }}
        .stat-card .value {{ font-size: 2rem; font-weight: 700; }}
        .stat-card.success .value {{ color: var(--success); }}
        .stat-card.danger .value {{ color: var(--danger); }}
        .stat-card.warn .value {{ color: var(--warn); }}
        .stat-card.accent .value {{ color: var(--accent); }}
        .search-form {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; margin-bottom: 2rem; }}
        .search-form h2 {{ margin-bottom: 1rem; font-size: 1.25rem; }}
        .form-row {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; margin-bottom: 1rem; }}
        .form-group {{ display: flex; flex-direction: column; }}
        .form-group label {{ font-size: 0.875rem; color: #8b949e; margin-bottom: 0.25rem; }}
        .form-group input, .form-group select {{ background: var(--bg); border: 1px solid var(--border); border-radius: 6px; padding: 0.5rem; color: var(--text); font-size: 0.875rem; }}
        .form-group button {{ background: var(--accent); color: white; border: none; border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-size: 0.875rem; font-weight: 600; }}
        .form-group button:hover {{ opacity: 0.9; }}
        table {{ width: 100%; border-collapse: collapse; background: var(--card); border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }}
        th, td {{ padding: 0.75rem 1rem; text-align: left; border-bottom: 1px solid var(--border); }}
        th {{ background: rgba(88, 166, 255, 0.1); font-weight: 600; font-size: 0.875rem; text-transform: uppercase; letter-spacing: 0.05em; }}
        tr:hover {{ background: rgba(255,255,255,0.03); }}
        .empty {{ text-align: center; padding: 3rem; color: #8b949e; }}
        .empty img {{ display: block; margin: 0 auto .75rem; width: 96px; height: 84px; image-rendering: pixelated; }}
        .empty.error {{ color: #f85149; }}
        .severity-badge {{ display: inline-block; padding: 0.125rem 0.5rem; border-radius: 12px; font-size: 0.75rem; font-weight: 600; }}
        .severity-critical {{ background: rgba(248, 81, 73, 0.2); color: #f85149; }}
        .severity-warning {{ background: rgba(210, 153, 34, 0.2); color: #d29922; }}
        .severity-info {{ background: rgba(88, 166, 255, 0.2); color: #58a6ff; }}
    </style>
</head>
<body>
    <div class="container">
        <header>
            <h1>🦞 SentryClaw Dashboard</h1>
            <p>Code review analytics and history</p>
        </header>
        <div class="stats">
            <div class="stat-card accent">
                <h3>Total Reviews</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card success">
                <h3>Approved</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card danger">
                <h3>Changes Requested</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card warn">
                <h3>Commented</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card accent">
                <h3>Avg Inline Comments</h3>
                <div class="value">{:.1}</div>
            </div>
        </div>
        <div class="stats">
            <div class="stat-card danger">
                <h3>Critical Findings</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card warn">
                <h3>Warning Findings</h3>
                <div class="value">{}</div>
            </div>
            <div class="stat-card accent">
                <h3>Info Findings</h3>
                <div class="value">{}</div>
            </div>
        </div>
        <div class="search-form">
            <h2>Search Reviews</h2>
            <form id="searchForm" onsubmit="return handleSearch(event)">
                <div class="form-row">
                    <div class="form-group">
                        <label for="repo">Repository</label>
                        <input type="text" id="repo" name="repo" placeholder="owner/repo">
                    </div>
                    <div class="form-group">
                        <label for="verdict">Verdict</label>
                        <select id="verdict" name="verdict">
                            <option value="">All</option>
                            <option value="Approve">Approve</option>
                            <option value="RequestChanges">Request Changes</option>
                            <option value="Comment">Comment</option>
                        </select>
                    </div>
                    <div class="form-group">
                        <label for="from">From</label>
                        <input type="datetime-local" id="from" name="from">
                    </div>
                    <div class="form-group">
                        <label for="to">To</label>
                        <input type="datetime-local" id="to" name="to">
                    </div>
                </div>
                <div class="form-row">
                    <div class="form-group">
                        <button type="submit">Search</button>
                    </div>
                </div>
            </form>
        </div>
        <h2 style="margin-bottom:1rem">Recent Reviews</h2>
        <table>
            <thead>
                <tr><th>Time</th><th>Provider</th><th>PR/MR</th><th>Verdict</th><th>Inline</th><th>Summary</th></tr>
            </thead>
            <tbody id="reviewsTable">
                {}
            </tbody>
        </table>
    </div>
    <script>
        function handleSearch(e) {{
            e.preventDefault();
            const params = new URLSearchParams();
            const repo = document.getElementById('repo').value;
            const verdict = document.getElementById('verdict').value;
            const from = document.getElementById('from').value;
            const to = document.getElementById('to').value;
            if (repo) params.append('repo', repo);
            if (verdict) params.append('verdict', verdict);
            if (from) params.append('from', new Date(from).toISOString());
            if (to) params.append('to', new Date(to).toISOString());
            params.append('limit', '50');
            
            const tbody = document.getElementById('reviewsTable');
            const knightRow = (state, msg) => `<tr><td colspan="6" class="empty${{state === 'error' ? ' error' : ''}}"><img src="/dashboard/knight/${{state}}" alt="">${{msg}}</td></tr>`;
            tbody.innerHTML = knightRow('thinking', 'Searching reviews...');

            fetch('/dashboard/api/search?' + params.toString())
                .then(r => r.json())
                .then(data => {{
                    if (data.length === 0) {{
                        tbody.innerHTML = knightRow('idle', 'No reviews found matching your criteria.');
                        return;
                    }}
                    tbody.innerHTML = data.map(r => {{
                        const emoji = r.verdict === 'Approve' ? '✅' : r.verdict === 'RequestChanges' ? '❌' : '💬';
                        const summary = (r.summary || '').substring(0, 100);
                        return `<tr><td>${{new Date(r.created_at).toLocaleString()}}</td><td>${{r.provider}}</td><td>${{r.repo}}#${{r.pr_number}}</td><td>${{emoji}} ${{r.verdict}}</td><td>${{r.inline_count}}</td><td>${{summary}}</td></tr>`;
                    }}).join('');
                }})
                .catch(err => {{
                    console.error('Search failed:', err);
                    tbody.innerHTML = knightRow('error', 'Search failed. Check the server log.');
                }});
            return false;
        }}
        setTimeout(() => location.reload(), {}000);
    </script>
</body>
</html>"#,
        stats.total_reviews,
        stats.approved,
        stats.request_changes,
        stats.commented,
        stats.avg_inline_comments,
        stats.critical_count,
        stats.warning_count,
        stats.info_count,
        if rows.is_empty() {
            "<tr><td colspan=\"6\" class=\"empty\"><img src=\"/dashboard/knight/idle\" alt=\"\">No reviews yet. Start reviewing some code!</td></tr>".to_string()
        } else {
            rows
        },
        refresh_seconds
    );

    Ok(html)
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod knight_tests {
    use super::knight_svg;

    #[test]
    fn knight_states_are_embedded_svgs() {
        for state in ["idle", "thinking", "error"] {
            assert!(knight_svg(state).unwrap().starts_with("<svg"), "{state}");
        }
        assert!(knight_svg("nope").is_none());
    }
}
