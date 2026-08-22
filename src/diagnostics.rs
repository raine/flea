use std::path::PathBuf;

#[derive(Debug)]
pub struct DiagnosticsContext {
    pub trace_id: String,
    pub log_path: PathBuf,
}
