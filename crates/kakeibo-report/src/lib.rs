//! Table, Markdown, JSON, NDJSON, and flag text renderers plus TUI widgets.

pub mod flags;
pub mod json_export;
pub mod markdown;
pub mod model;
pub mod ndjson;
pub mod table;

pub use json_export::{build_json_export_v1, render_json_export_v1, write_json_export_v1};
pub use markdown::render_markdown_export;
pub use model::{
    share_basis_points, sort_session_reports, CostStatus, CostSummary, DoctorReport,
    DoctorSourceSummary, SessionReport, ToolSpendSummary, TopDimension, UnknownTopDimensionError,
};
pub use table::{
    render_autopsy_report, render_cross_session_flags, render_doctor_report, render_sessions_table,
    render_top_table,
};
