use crate::error::{AircError, Result};
use tracing::warn;

const MAX_RETRY_ATTEMPTS: u32 = 3;
const RETRY_DELAY_MS: u64 = 1000;

// Retry a network operation with exponential backoff
pub(crate) async fn retry_with_backoff<F, Fut, T>(operation: F, operation_name: &str) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) if attempts >= MAX_RETRY_ATTEMPTS => {
                return Err(AircError::Connection(format!(
                    "{} failed after {} attempts: {}",
                    operation_name, MAX_RETRY_ATTEMPTS, e
                )));
            }
            Err(e) => {
                let delay = RETRY_DELAY_MS * 2u64.pow(attempts - 1);
                warn!(
                    "{} attempt {}/{} failed: {}. Retrying in {}ms...",
                    operation_name, attempts, MAX_RETRY_ATTEMPTS, e, delay
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }
        }
    }
}
