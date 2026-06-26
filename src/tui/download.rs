//! Per-download progress state backing the TUI download gauges.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DownloadStatus {
    Starting,
    InProgress,
    Completed,
    Failed(String),
    Extracting,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadProgress {
    pub filename: String,
    pub progress: u16, // 0-100, drives the Gauge widget
    pub total_size: u64,
    pub current_size: u64,
    pub status: DownloadStatus,
}

impl DownloadProgress {
    pub fn new(filename: String, total_size: u64) -> Self {
        Self {
            filename,
            progress: 0,
            total_size,
            current_size: 0,
            status: DownloadStatus::Starting,
        }
    }

    pub fn update_progress(&mut self, current_size: u64) {
        self.current_size = current_size;
        if self.total_size > 0 {
            let pct = ((current_size as f64 / self.total_size as f64) * 100.0).round() as u16;
            self.progress = pct.min(100);
        }
        self.status = DownloadStatus::InProgress;
    }

    pub fn mark_completed(&mut self) {
        self.status = DownloadStatus::Completed;
        self.progress = 100;
    }

    pub fn mark_failed(&mut self, error: String) {
        self.status = DownloadStatus::Failed(error);
    }

    pub fn mark_extracting(&mut self) {
        self.status = DownloadStatus::Extracting;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_percent_half() {
        let mut d = DownloadProgress::new("a.txt".to_string(), 200);
        d.update_progress(100);
        assert_eq!(d.progress, 50);
        assert_eq!(d.status, DownloadStatus::InProgress);
    }

    #[test]
    fn test_progress_percent_zero_size() {
        // A zero-size download must never divide by zero; percent stays 0.
        let mut d = DownloadProgress::new("empty".to_string(), 0);
        d.update_progress(0);
        assert_eq!(d.progress, 0);
    }

    #[test]
    fn test_progress_clamps_over_100() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.update_progress(150);
        assert_eq!(d.progress, 100);
    }

    #[test]
    fn test_mark_completed_sets_full() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.mark_completed();
        assert_eq!(d.progress, 100);
        assert_eq!(d.status, DownloadStatus::Completed);
    }

    #[test]
    fn test_mark_failed_and_extracting() {
        let mut d = DownloadProgress::new("a".to_string(), 100);
        d.mark_failed("boom".to_string());
        assert_eq!(d.status, DownloadStatus::Failed("boom".to_string()));
        d.mark_extracting();
        assert_eq!(d.status, DownloadStatus::Extracting);
    }
}
