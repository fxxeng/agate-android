//! 有界上游响应观测。只含状态、耗时、字节数与出口类别，不含响应正文或凭证。

#[derive(Clone, Debug, Default)]
pub struct ResponseObservation {
    pub status: Option<u16>,
    pub elapsed_ms: u64,
    pub response_bytes: u64,
    pub egress: &'static str,
}

impl ResponseObservation {
    pub fn log_line(&self, operation: &str) -> String {
        format!(
            "{operation} → status={} elapsed={}ms bytes={} egress={}",
            self.status
                .map_or_else(|| "transport_error".to_string(), |s| s.to_string()),
            self.elapsed_ms,
            self.response_bytes,
            self.egress,
        )
    }
}
