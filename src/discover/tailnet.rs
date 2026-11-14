//! Tailnet peers from `tailscale status --json`.

use serde::Deserialize;
use std::collections::HashMap;

/// One tailnet peer that could run herdr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailnetPeer {
    /// The machine's own host name (`HostName`).
    pub host_name: String,
    /// MagicDNS name without the trailing dot: the SSH target.
    pub dns_name: String,
    pub os: Option<String>,
    pub online: bool,
    pub ips: Vec<String>,
}

impl TailnetPeer {
    /// The first label of the MagicDNS name, as `tailscale status` shows it.
    pub fn short_name(&self) -> &str {
        self.dns_name.split('.').next().unwrap_or(&self.dns_name)
    }
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "Peer", default)]
    peer: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName", default)]
    host_name: Option<String>,
    #[serde(rename = "DNSName", default)]
    dns_name: Option<String>,
    #[serde(rename = "OS", default)]
    os: Option<String>,
    #[serde(rename = "Online", default)]
    online: Option<bool>,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<String>>,
}

/// Peer OSes that cannot run herdr.
const NO_HERDR_OS: &[&str] = &["ios", "ipados", "android", "tvos", "watchos"];

/// Parse `tailscale status --json`. `Self` is skipped, and so are phones and
/// tablets. A peer that does not parse is skipped; output that does not parse
/// yields nothing. Sorted by DNS name.
pub fn parse_tailscale_status(json: &str) -> Vec<TailnetPeer> {
    let Ok(status) = serde_json::from_str::<Status>(json) else {
        return Vec::new();
    };
    let mut peers: Vec<TailnetPeer> = status
        .peer
        .unwrap_or_default()
        .into_values()
        .filter_map(|v| serde_json::from_value::<RawPeer>(v).ok())
        .filter_map(|p| {
            let os = p.os.filter(|s| !s.is_empty());
            if os
                .as_deref()
                .is_some_and(|os| NO_HERDR_OS.contains(&os.to_ascii_lowercase().as_str()))
            {
                return None;
            }
            let ips = p.ips.unwrap_or_default();
            let dns_name = p
                .dns_name
                .map(|d| d.trim_end_matches('.').to_string())
                .filter(|d| !d.is_empty())
                .or_else(|| ips.first().cloned())?;
            Some(TailnetPeer {
                host_name: p.host_name.unwrap_or_default(),
                dns_name,
                os,
                online: p.online.unwrap_or(false),
                ips,
            })
        })
        .collect();
    peers.sort_by(|a, b| a.dns_name.cmp(&b.dns_name));
    peers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic, shaped like real `tailscale status --json` output.
    const FIXTURE: &str = r#"{
      "Version": "1.90.0-t0000000-g0000000",
      "TUN": true,
      "BackendState": "Running",
      "TailscaleIPs": ["100.64.0.1", "fd7a:115c:a1e0::1"],
      "Self": {
        "ID": "nSelf", "HostName": "laptop", "DNSName": "laptop.tail0000.ts.net.",
        "OS": "linux", "Online": true, "TailscaleIPs": ["100.64.0.1"]
      },
      "MagicDNSSuffix": "tail0000.ts.net",
      "CurrentTailnet": {"Name": "example.org", "MagicDNSSuffix": "tail0000.ts.net"},
      "Peer": {
        "nodekey:aaa": {
          "ID": "n1", "PublicKey": "nodekey:aaa", "HostName": "Work-Mac",
          "DNSName": "work-mac.tail0000.ts.net.", "OS": "macOS",
          "UserID": 1, "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::2"],
          "Online": true, "ExitNode": false, "Tags": null, "CapMap": {}
        },
        "nodekey:bbb": {
          "ID": "n2", "HostName": "gaming-pc", "DNSName": "gaming-pc.tail0000.ts.net.",
          "OS": "windows", "TailscaleIPs": ["100.64.0.3"], "Online": false
        },
        "nodekey:ccc": {
          "ID": "n3", "HostName": "localhost", "DNSName": "phone.tail0000.ts.net.",
          "OS": "iOS", "TailscaleIPs": ["100.64.0.4"], "Online": true
        },
        "nodekey:ddd": {
          "ID": "n4", "HostName": "pixel", "DNSName": "pixel.tail0000.ts.net.",
          "OS": "android", "TailscaleIPs": ["100.64.0.5"], "Online": false
        },
        "nodekey:eee": {
          "ID": "n5", "HostName": "server", "DNSName": "server.tail0000.ts.net.",
          "OS": "linux", "TailscaleIPs": ["100.64.0.6"], "Online": true,
          "SomeFutureField": {"x": [1, 2]}
        },
        "nodekey:fff": {"ID": "n6", "HostName": 42},
        "nodekey:ggg": {"ID": "n7", "HostName": "nodns", "DNSName": "", "OS": "linux",
          "TailscaleIPs": ["100.64.0.8"]}
      },
      "User": {"1": {"ID": 1, "LoginName": "someone@example.org"}}
    }"#;

    #[test]
    fn parses_peers() {
        let peers = parse_tailscale_status(FIXTURE);
        let names: Vec<&str> = peers.iter().map(|p| p.dns_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "100.64.0.8",
                "gaming-pc.tail0000.ts.net",
                "server.tail0000.ts.net",
                "work-mac.tail0000.ts.net"
            ]
        );
        let mac = &peers[3];
        assert_eq!(mac.host_name, "Work-Mac");
        assert_eq!(mac.short_name(), "work-mac");
        assert_eq!(mac.os.as_deref(), Some("macOS"));
        assert!(mac.online);
        assert_eq!(mac.ips, ["100.64.0.2", "fd7a:115c:a1e0::2"]);
        let win = &peers[1];
        assert_eq!(win.os.as_deref(), Some("windows"));
        assert!(!win.online);
        assert!(!peers.iter().any(|p| p.dns_name.starts_with("laptop")));
    }

    #[test]
    fn bad_output_is_empty() {
        assert!(parse_tailscale_status("").is_empty());
        assert!(parse_tailscale_status("not json").is_empty());
        assert!(parse_tailscale_status("[]").is_empty());
        assert!(parse_tailscale_status(r#"{"Peer": null}"#).is_empty());
        assert!(parse_tailscale_status(r#"{"BackendState": "NeedsLogin"}"#).is_empty());
    }
}
