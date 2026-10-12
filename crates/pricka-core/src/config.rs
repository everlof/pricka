/// Proxy configuration for bank connections.
#[derive(Debug, Clone)]
pub enum ProxyConfig {
    /// No proxy
    None,
    /// SOCKS5 proxy (e.g., Tor: socks5://127.0.0.1:9050)
    Socks5(String),
    /// HTTP proxy
    Http(String),
}
