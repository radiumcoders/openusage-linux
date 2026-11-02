//! Refresh cadence, the same constants as the Mac app's `RefreshSetting`.

pub const INTERVAL_SECONDS: i64 = 5 * 60;
/// A forced refresh lets the next pass skip an account, so working refreshes can leave its values about
/// two intervals old.
pub const STALENESS_THRESHOLD_SECONDS: i64 = INTERVAL_SECONDS * 2;
pub const FAILURE_BACKOFF_BASE_SECONDS: i64 = INTERVAL_SECONDS;
pub const FAILURE_BACKOFF_CAP_SECONDS: i64 = 60 * 60;
pub const SECONDARY_READ_INTERVAL_SECONDS: i64 = 60 * 60;
/// One account's read is abandoned after this long.
pub const PROVIDER_TIMEOUT_SECONDS: u64 = 120;
