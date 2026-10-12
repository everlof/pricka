use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Generate a BankID QR code data string.
///
/// Format: `bankid.{qrStartToken}.{elapsed_seconds}.{HMAC-SHA256(qrStartSecret, elapsed_seconds)}`
///
/// The QR code updates every second — `elapsed_seconds` is the number of
/// seconds since the auth order was created.
pub fn generate_qr_data(
    qr_start_token: &str,
    qr_start_secret: &str,
    elapsed_seconds: u64,
) -> String {
    let time_str = elapsed_seconds.to_string();

    let mut mac =
        HmacSha256::new_from_slice(qr_start_secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(time_str.as_bytes());
    let hash = hex::encode(mac.finalize().into_bytes());

    format!("bankid.{qr_start_token}.{elapsed_seconds}.{hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_format() {
        let data = generate_qr_data("some-token", "some-secret", 0);
        assert!(data.starts_with("bankid.some-token.0."));
        // Hash should be 64 hex chars (SHA-256)
        let parts: Vec<&str> = data.split('.').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[3].len(), 64);
    }

    #[test]
    fn test_qr_changes_with_time() {
        let data0 = generate_qr_data("token", "secret", 0);
        let data1 = generate_qr_data("token", "secret", 1);
        assert_ne!(data0, data1);
    }
}
