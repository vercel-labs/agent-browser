//! Where a flow's direct requests may land: only addresses of the class the
//! flow was recorded against, checked at connect time so the address that
//! was checked is the address that is used.

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Which addresses a flow may send to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    /// Public addresses only.
    Public,
    /// Loopback, private and link-local addresses only: the flow was
    /// recorded against a local target.
    Local,
}

impl Reach {
    /// A start URL on `localhost`, `*.localhost` or a non-public IP literal
    /// was recorded against a local target; anything else is public.
    pub fn of_start(start_url: &str) -> Reach {
        let Some(url) = url::Url::parse(start_url).ok() else {
            return Reach::Public;
        };
        let local = match url.host() {
            Some(url::Host::Domain(d)) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(ip)) => is_non_public(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => is_non_public(IpAddr::V6(ip)),
            None => false,
        };
        if local {
            Reach::Local
        } else {
            Reach::Public
        }
    }

    /// The reach of a saved flow: the one its recording found, else the
    /// one its start URL names.
    pub fn of_flow(flow: &super::Flow) -> Reach {
        flow.reach
            .unwrap_or_else(|| Reach::of_start(&flow.start_url))
    }

    fn admits(self, ip: IpAddr, non_public: fn(IpAddr) -> bool) -> bool {
        non_public(ip) == (self == Reach::Local)
    }

    fn refusal(self, host: &str, ip: IpAddr) -> String {
        match self {
            Reach::Public => format!(
                "{host} resolved to a non-public address ({ip}); flow run does not send requests there"
            ),
            Reach::Local => format!(
                "{host} resolved to a public address ({ip}); a flow recorded against a local target runs only against local addresses"
            ),
        }
    }
}

/// True for every address a public site has no business resolving to.
pub fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => non_public_v4(v4),
        IpAddr::V6(v6) => non_public_v6(v6),
    }
}

fn non_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 0.0.0.0/8 "this network"
        || a == 0
        // 100.64.0.0/10 carrier-grade NAT
        || (a == 100 && (b & 0xc0) == 64)
        // 192.0.0.0/24 IETF protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // documentation ranges
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // 198.18.0.0/15 benchmarking
        || (a == 198 && (b & 0xfe) == 18)
        // 240.0.0.0/4 reserved
        || a >= 240
}

fn non_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let v4 = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    // IPv4 carried inside the address: mapped, SIIT, compatible (and `::`,
    // `::1`), NAT64, 6to4.
    let low_v4 = (s[..5] == [0; 5] && s[5] == 0xffff)
        || (s[..4] == [0; 4] && s[4] == 0xffff && s[5] == 0)
        || s[..6] == [0; 6]
        || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    let embedded = if low_v4 {
        Some(v4(s[6], s[7]))
    } else if s[0] == 0x2002 {
        Some(v4(s[1], s[2]))
    } else {
        None
    };
    if let Some(v4) = embedded {
        return non_public_v4(v4);
    }
    ip.is_multicast()
        // fc00::/7 unique local
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local, fec0::/10 site-local (deprecated)
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        // 64:ff9b:1::/48 local-use NAT64 (RFC 8215)
        || s[..3] == [0x64, 0xff9b, 1]
        // 100::/64 discard
        || (s[0] == 0x100 && s[1..4] == [0; 3])
}

/// The address to pin a proxy to: the first IPv4 address if there is one,
/// else the first address. reqwest takes one host per proxy, so there is no
/// failover between addresses.
pub fn pick_address(addrs: Vec<IpAddr>) -> Option<IpAddr> {
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .copied()
}

/// `host` when the browser reached it only at non-public addresses that the
/// name still resolves to now, on the URL's own port, and the name resolves
/// to nothing else. Through any proxy (launch flag, system setting, an
/// external Chrome) the browser records the proxy's address and port, so a
/// host reached through one is never local.
pub async fn recorded_local_host(host: &str, port: u16, recorded: &[SocketAddr]) -> Option<String> {
    let looked = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::lookup_host((host, 0)),
    )
    .await;
    let resolved: Vec<IpAddr> = match looked {
        Ok(Ok(addrs)) => addrs.map(|a| a.ip()).collect(),
        _ => return None,
    };
    local_recording(recorded, port, &resolved).then(|| host.to_string())
}

/// Every recorded address is non-public, among `resolved` and on `port`, and
/// every resolved address is non-public.
pub fn local_recording(recorded: &[SocketAddr], port: u16, resolved: &[IpAddr]) -> bool {
    !recorded.is_empty()
        && !resolved.is_empty()
        && resolved.iter().all(|ip| is_non_public(*ip))
        && recorded.iter().all(|a| {
            let ip = a.ip().to_canonical();
            a.port() == port && is_non_public(ip) && resolved.contains(&ip)
        })
}

/// Resolve the hostnames of the `http://` proxies, with `limit` as the time
/// each lookup may take. A failure is kept per name so the build can fail
/// closed with the proxy named.
pub async fn resolve_proxy_hosts(
    proxies: &[EnvProxy],
    limit: std::time::Duration,
) -> std::collections::HashMap<(String, u16), Result<IpAddr, String>> {
    let mut found = std::collections::HashMap::new();
    for p in proxies {
        let Some(url::Host::Domain(name)) = p.url.host() else {
            continue;
        };
        if p.url.scheme() != "http" {
            continue;
        }
        let port = p.url.port_or_known_default().unwrap_or(80);
        let key = (name.to_string(), port);
        if found.contains_key(&key) {
            continue;
        }
        let looked = tokio::time::timeout(limit, tokio::net::lookup_host((name, port))).await;
        let result = match looked {
            Ok(Ok(addrs)) => pick_address(addrs.map(|a| a.ip()).collect())
                .ok_or_else(|| format!("could not resolve proxy {name}: no addresses")),
            Ok(Err(e)) => Err(format!("could not resolve proxy {name}: {e}")),
            Err(_) => Err(format!(
                "could not resolve proxy {name}: no answer within {}s",
                limit.as_secs()
            )),
        };
        found.insert(key, result);
    }
    found
}

/// Proxy hostnames the resolver treats apart from targets.
#[derive(Debug, Default)]
pub struct ProxyNames {
    /// Proxies from the user's configuration: trusted, never filtered.
    pub trusted: Vec<String>,
    /// `http://` proxies whose name did not resolve, with the reason: a
    /// request through one fails with it, and one `NO_PROXY` excludes runs.
    pub unreachable: Vec<(String, String)>,
}

/// The reqwest proxies for the environment, honoring `NO_PROXY` for which
/// targets use them, and the proxy hostnames the resolver handles apart.
pub fn client_proxies(
    env: impl Fn(&str) -> Option<String>,
    no_proxy: Option<reqwest::NoProxy>,
    lookup: impl Fn(&str, u16) -> Result<IpAddr, String>,
) -> Result<(Vec<reqwest::Proxy>, ProxyNames), String> {
    let mut proxies = Vec::new();
    let mut names = ProxyNames::default();
    for p in env_proxies(env)? {
        let (url, name) = match pin_proxy(p.url.clone(), &lookup) {
            Ok(pinned) => pinned,
            Err(reason) => {
                let host = p.url.host_str().unwrap_or("");
                names
                    .unreachable
                    .push((host.trim_end_matches('.').to_ascii_lowercase(), reason));
                (p.url, None)
            }
        };
        names.trusted.extend(name);
        let proxy = if p.for_https {
            reqwest::Proxy::https(url.as_str())
        } else {
            reqwest::Proxy::http(url.as_str())
        }
        .map_err(|e| format!("invalid proxy {}: {}", url, e))?;
        proxies.push(proxy.no_proxy(no_proxy.clone()));
    }
    Ok((proxies, names))
}

/// A destination the resolver refused.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Refused {}

/// The refusal inside a request error, if the resolver caused it.
pub fn refusal_in(err: &(dyn Error + 'static)) -> Option<String> {
    let mut cur: Option<&(dyn Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(r) = e.downcast_ref::<Refused>() {
            return Some(r.0.clone());
        }
        cur = e.source();
    }
    None
}

/// The addresses a flow may use out of what a name resolved to.
pub fn admit(
    host: &str,
    addrs: Vec<SocketAddr>,
    reach: Reach,
    non_public: fn(IpAddr) -> bool,
) -> Result<Vec<SocketAddr>, Refused> {
    let first_refused = addrs
        .iter()
        .find(|a| !reach.admits(a.ip(), non_public))
        .map(|a| a.ip());
    let kept: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| reach.admits(a.ip(), non_public))
        .collect();
    match (kept.is_empty(), first_refused) {
        (true, Some(ip)) => Err(Refused(reach.refusal(host, ip))),
        (true, None) => Err(Refused(format!("{host} did not resolve to any address"))),
        _ => Ok(kept),
    }
}

/// Resolves with `inner` and hands reqwest only the admitted addresses, so
/// the connection goes to an address that was checked.
pub struct ReachResolver {
    pub inner: Arc<dyn Resolve>,
    pub reach: Reach,
    pub non_public: fn(IpAddr) -> bool,
    /// Proxy hosts from the user's configuration: trusted, never filtered.
    pub trusted: Vec<String>,
    /// Proxy hosts that did not resolve, refused with the reason.
    pub unreachable: Vec<(String, String)>,
}

impl ReachResolver {
    pub fn system(reach: Reach) -> ReachResolver {
        ReachResolver {
            inner: Arc::new(SystemResolver),
            reach,
            non_public: is_non_public,
            trusted: Vec::new(),
            unreachable: Vec::new(),
        }
    }
}

/// A proxy the environment configures, for http or https targets.
#[derive(Debug, PartialEq)]
pub struct EnvProxy {
    pub for_https: bool,
    pub url: url::Url,
}

/// The proxies `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` configure, as
/// reqwest reads them: `ALL_PROXY` covers a scheme with no proxy of its own.
pub fn env_proxies(env: impl Fn(&str) -> Option<String>) -> Result<Vec<EnvProxy>, String> {
    // httpoxy: a CGI request can set HTTP_PROXY, so none is read there.
    if env("REQUEST_METHOD").is_some() {
        return Ok(Vec::new());
    }
    // As hyper-util reads them: the first variable set wins, even if empty.
    let get = |name: &str| {
        [name.to_ascii_uppercase(), name.to_ascii_lowercase()]
            .iter()
            .find_map(|k| env(k))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let all = get("ALL_PROXY");
    let mut out = Vec::new();
    for (for_https, own) in [(false, get("HTTP_PROXY")), (true, get("HTTPS_PROXY"))] {
        let Some(raw) = own.or_else(|| all.clone()) else {
            continue;
        };
        let full = if raw.contains("://") {
            raw.clone()
        } else {
            format!("http://{raw}")
        };
        let url = url::Url::parse(&full).map_err(|e| format!("invalid proxy '{raw}': {e}"))?;
        out.push(EnvProxy { for_https, url });
    }
    Ok(out)
}

/// An `http://` proxy named by a hostname is resolved once and used by
/// address, so its connection never goes through the resolver's filter and
/// the filter keeps no exception that a target could share. Other proxies
/// are returned unchanged, with the hostname the resolver must trust.
pub fn pin_proxy(
    mut url: url::Url,
    lookup: impl Fn(&str, u16) -> Result<IpAddr, String>,
) -> Result<(url::Url, Option<String>), String> {
    let Some(url::Host::Domain(name)) = url.host().map(|h| h.to_owned()) else {
        return Ok((url, None));
    };
    if url.scheme() != "http" {
        let trusted = name.trim_end_matches('.').to_ascii_lowercase();
        return Ok((url, Some(trusted)));
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let ip = lookup(&name, port)?;
    let _ = url.set_ip_host(ip);
    Ok((url, None))
}

impl Resolve for ReachResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let (reach, non_public) = (self.reach, self.non_public);
        let trusted = self
            .trusted
            .iter()
            .any(|t| t.eq_ignore_ascii_case(host.trim_end_matches('.')));
        if let Some((_, reason)) = self
            .unreachable
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(host.trim_end_matches('.')))
        {
            let refused = Refused(reason.clone());
            return Box::pin(async move { Err(Box::new(refused) as Box<dyn Error + Send + Sync>) });
        }
        let inner = self.inner.resolve(name);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = inner.await?.collect();
            if trusted {
                return Ok(Box::new(addrs.into_iter()) as Addrs);
            }
            let kept = admit(&host, addrs, reach, non_public)?;
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}

struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(Box::new(addrs.collect::<Vec<_>>().into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::flow::{flow_of, http_client_with, run_http, PreparedRequest};
    use serde_json::json;
    use std::str::FromStr;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifier_table() {
        let non_public = [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.5",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::1",
            "64:ff9b:1::1",
            "64:ff9b:1:ffff::808:808",
            "::ffff:0:127.0.0.1",
            "::ffff:0:10.0.0.5",
            "::ffff:0:169.254.169.254",
            "100::",
            "100::1",
            "2002:0a00:0001::",
        ];
        for s in non_public {
            assert!(is_non_public(ip(s)), "{s} should be non-public");
        }
        let public = [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.169.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "2606:4700::1111",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "::ffff:0:8.8.8.8",
            "100:0:0:1::1",
            "101::1",
            "2002:0808:0808::1",
        ];
        for s in public {
            assert!(!is_non_public(ip(s)), "{s} should be public");
        }
    }

    #[test]
    fn reach_follows_the_start_url() {
        for (url, reach) in [
            ("https://shop.example.com/", Reach::Public),
            ("https://8.8.8.8/", Reach::Public),
            ("http://localhost:3000/", Reach::Local),
            ("http://LOCALHOST./", Reach::Local),
            ("http://app.localhost:3000/", Reach::Local),
            ("http://127.0.0.1:8080/", Reach::Local),
            ("http://192.168.1.20/", Reach::Local),
            ("http://[::1]:3000/", Reach::Local),
            ("http://localhost.example.com/", Reach::Public),
            ("http://notlocalhost/", Reach::Public),
            ("not a url", Reach::Public),
        ] {
            assert_eq!(Reach::of_start(url), reach, "{url}");
        }
    }

    fn sock(s: &str) -> SocketAddr {
        SocketAddr::new(ip(s), 0)
    }

    #[test]
    fn admit_filters_addresses() {
        let r = admit(
            "a.test",
            vec![sock("10.0.0.5")],
            Reach::Public,
            is_non_public,
        );
        assert_eq!(
            r.unwrap_err().0,
            "a.test resolved to a non-public address (10.0.0.5); flow run does not send requests there"
        );
        let kept = admit(
            "a.test",
            vec![sock("127.0.0.1"), sock("93.184.216.34")],
            Reach::Public,
            is_non_public,
        )
        .unwrap();
        assert_eq!(kept, vec![sock("93.184.216.34")]);
        assert!(admit("a.test", vec![], Reach::Public, is_non_public).is_err());
        // Local flows stay local.
        assert!(admit(
            "localhost",
            vec![sock("127.0.0.1")],
            Reach::Local,
            is_non_public
        )
        .is_ok());
        assert!(admit(
            "localhost",
            vec![sock("8.8.8.8")],
            Reach::Local,
            is_non_public
        )
        .is_err());
    }

    struct Fixed(Vec<SocketAddr>);
    impl Resolve for Fixed {
        fn resolve(&self, _name: Name) -> Resolving {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(Box::new(addrs.into_iter()) as Addrs) })
        }
    }

    #[tokio::test]
    async fn resolver_filters() {
        let name = || Name::from_str("a.test").unwrap();
        let rebound = ReachResolver {
            inner: Arc::new(Fixed(vec![sock("127.0.0.1")])),
            reach: Reach::Public,
            non_public: is_non_public,
            trusted: vec![],
            unreachable: vec![],
        };
        let err = match rebound.resolve(name()).await {
            Ok(_) => panic!("loopback must be refused"),
            Err(e) => e,
        };
        assert!(refusal_in(err.as_ref()).unwrap().contains("127.0.0.1"));
        let ok = ReachResolver {
            inner: Arc::new(Fixed(vec![sock("93.184.216.34")])),
            reach: Reach::Public,
            non_public: is_non_public,
            trusted: vec![],
            unreachable: vec![],
        };
        assert_eq!(ok.resolve(name()).await.unwrap().count(), 1);
    }

    // Test seam: 127.0.0.1 is the only address treated as public.
    fn loopback_is_public(ip: IpAddr) -> bool {
        ip != IpAddr::from([127, 0, 0, 1]) && is_non_public(ip)
    }

    /// Answers every request with `reply` and counts the requests.
    async fn serve(reply: String) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(reply.as_bytes()).await;
                });
            }
        });
        (port, hits)
    }

    fn request(url: &str) -> PreparedRequest {
        PreparedRequest {
            id: "r".into(),
            step: 1,
            method: "GET".into(),
            url: url.into(),
            headers: vec![],
            body: None,
            expect: Default::default(),
            write: false,
            parts: None,
            repeated_headers: vec![],
        }
    }

    fn client_for(
        start: &str,
        mapped: Vec<SocketAddr>,
        non_public: fn(IpAddr) -> bool,
    ) -> crate::native::flow::HttpClients {
        let flow = flow_of(json!({ "startUrl": start, "origin": "example.com" }));
        let resolver = ReachResolver {
            inner: Arc::new(Fixed(mapped)),
            reach: Reach::of_start(start),
            non_public,
            trusted: vec![],
            unreachable: vec![],
        };
        http_client_with(None, &flow, Arc::new(resolver), vec![]).unwrap()
    }

    const OK: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}";

    #[tokio::test]
    async fn rebound_name_is_refused_before_any_connection() {
        let (port, hits) = serve(OK.to_string()).await;
        let client = client_for(
            "https://app.example.com/",
            vec![SocketAddr::new(ip("127.0.0.1"), port)],
            is_non_public,
        );
        let res = run_http(
            &client,
            &request(&format!("http://app.example.com:{port}/")),
            None,
            None,
        )
        .await;
        assert!(!res.ok);
        let reason = res.reason.unwrap();
        assert!(
            reason.contains("resolved to a non-public address (127.0.0.1)"),
            "{reason}"
        );
        assert!(!res.drift);
        assert!(res.stop.is_none());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rebound_write_is_refused_without_being_sent() {
        let (port, hits) = serve(OK.to_string()).await;
        let client = client_for(
            "https://app.example.com/",
            vec![SocketAddr::new(ip("127.0.0.1"), port)],
            is_non_public,
        );
        let mut req = request(&format!("http://app.example.com:{port}/"));
        req.method = "POST".into();
        req.write = true;
        req.body = Some("{}".into());
        let res = run_http(&client, &req, None, None).await;
        let reason = res.reason.unwrap();
        assert!(reason.contains("non-public address"), "{reason}");
        assert!(res.stop.is_none(), "a refused write was never sent");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn redirect_to_a_name_that_resolves_to_loopback_is_refused() {
        // ok.example.com resolves to the test server, which the seam treats as
        // public; the redirect target resolves to ::1, which is not.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let reply = format!(
            "HTTP/1.1 302 Found\r\nlocation: http://evil.example.com:{port}/secret\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(reply.as_bytes()).await;
                });
            }
        });

        struct ByName(u16);
        impl Resolve for ByName {
            fn resolve(&self, name: Name) -> Resolving {
                let port = self.0;
                let addr = if name.as_str() == "evil.example.com" {
                    SocketAddr::new(IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]), port)
                } else {
                    SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port)
                };
                Box::pin(async move { Ok(Box::new(vec![addr].into_iter()) as Addrs) })
            }
        }
        let flow =
            flow_of(json!({ "startUrl": "https://ok.example.com/", "origin": "example.com" }));
        let resolver = ReachResolver {
            inner: Arc::new(ByName(port)),
            reach: Reach::Public,
            non_public: loopback_is_public,
            trusted: vec![],
            unreachable: vec![],
        };
        let client = http_client_with(None, &flow, Arc::new(resolver), vec![]).unwrap();
        let res = run_http(
            &client,
            &request(&format!("http://ok.example.com:{port}/")),
            None,
            None,
        )
        .await;
        assert!(!res.ok);
        let reason = res.reason.unwrap();
        assert!(
            reason.contains("evil.example.com resolved to a non-public address (::1)"),
            "{reason}"
        );
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the first hop connected"
        );
    }

    #[tokio::test]
    async fn local_flow_reaches_loopback() {
        let (port, hits) = serve(OK.to_string()).await;
        let client = client_for(
            "http://localhost:3000/",
            vec![SocketAddr::new(ip("127.0.0.1"), port)],
            is_non_public,
        );
        let res = run_http(
            &client,
            &request(&format!("http://localhost:{port}/")),
            None,
            None,
        )
        .await;
        assert!(
            !res.reason.unwrap_or_default().contains("resolved to"),
            "the resolver admitted loopback for a local flow"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn local_flow_does_not_reach_public_addresses() {
        let client = client_for(
            "http://localhost:3000/",
            vec![sock("8.8.8.8")],
            is_non_public,
        );
        let res = run_http(&client, &request("http://localhost:1/"), None, None).await;
        assert!(!res.ok);
        assert!(res
            .reason
            .unwrap()
            .contains("resolved to a public address (8.8.8.8)"));
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn env_proxies_follow_reqwest_precedence() {
        let p = env_proxies(env_of(&[
            ("HTTPS_PROXY", "http://User:pw@Proxy.Corp:3128"),
            ("all_proxy", "10.0.0.9:1080"),
        ]))
        .unwrap();
        let got: Vec<(bool, String)> = p.iter().map(|p| (p.for_https, p.url.to_string())).collect();
        assert_eq!(
            got,
            vec![
                (false, "http://10.0.0.9:1080/".to_string()),
                (true, "http://User:pw@proxy.corp:3128/".to_string()),
            ]
        );
        assert!(env_proxies(env_of(&[])).unwrap().is_empty());
        assert!(env_proxies(env_of(&[("HTTP_PROXY", "  ")]))
            .unwrap()
            .is_empty());
        assert!(env_proxies(env_of(&[("HTTP_PROXY", "http://[bad")])).is_err());
    }

    #[test]
    fn an_http_proxy_by_name_is_pinned_to_its_address() {
        let lookup = |h: &str, port: u16| {
            assert_eq!((h, port), ("proxy.corp", 3128));
            Ok(ip("10.1.2.3"))
        };
        let u = url::Url::parse("http://user:pw@proxy.corp:3128").unwrap();
        let (pinned, trusted) = pin_proxy(u, lookup).unwrap();
        assert_eq!(pinned.as_str(), "http://user:pw@10.1.2.3:3128/");
        assert!(trusted.is_none());
        let (same, _) = pin_proxy(
            url::Url::parse("http://127.0.0.1:7890").unwrap(),
            |_, _| panic!(),
        )
        .unwrap();
        assert_eq!(same.as_str(), "http://127.0.0.1:7890/");
        let (v6, _) = pin_proxy(url::Url::parse("http://proxy.corp:80").unwrap(), |_, _| {
            Ok(ip("fd00::1"))
        })
        .unwrap();
        assert_eq!(v6.host_str(), Some("[fd00::1]"));
        // https: the name stays for TLS, so the resolver trusts it.
        let (kept, trusted) = pin_proxy(
            url::Url::parse("https://proxy.corp:443").unwrap(),
            |_, _| panic!(),
        )
        .unwrap();
        assert_eq!(kept.host_str(), Some("proxy.corp"));
        assert_eq!(trusted.as_deref(), Some("proxy.corp"));
    }

    #[test]
    fn a_proxy_that_does_not_resolve_fails_closed_with_its_own_message() {
        let err = pin_proxy(url::Url::parse("http://nope.corp:80").unwrap(), |h, _| {
            Err(format!("could not resolve proxy {h}: no such host"))
        })
        .unwrap_err();
        assert!(
            err.starts_with("could not resolve proxy nope.corp"),
            "{err}"
        );
        let (_, names) = client_proxies(
            env_of(&[("HTTP_PROXY", "http://nope.corp:80")]),
            None,
            |h, _| Err(format!("could not resolve proxy {h}: no such host")),
        )
        .unwrap();
        assert_eq!(names.unreachable.len(), 1);
        assert_eq!(names.unreachable[0].0, "nope.corp");
        assert!(names.unreachable[0]
            .1
            .starts_with("could not resolve proxy nope.corp"));
    }

    #[test]
    fn pinning_prefers_the_first_ipv4_address() {
        let pick = |s: &[&str]| pick_address(s.iter().map(|a| ip(a)).collect());
        assert_eq!(
            pick(&["fd00::1", "10.0.0.1", "10.0.0.2"]),
            Some(ip("10.0.0.1"))
        );
        assert_eq!(pick(&["fd00::1", "fd00::2"]), Some(ip("fd00::1")));
        assert_eq!(pick(&[]), None);
    }

    #[tokio::test]
    async fn proxy_names_resolve_asynchronously_and_fail_closed() {
        let proxies = env_proxies(env_of(&[
            ("HTTP_PROXY", "http://localhost:3128"),
            ("HTTPS_PROXY", "https://secure.corp:443"),
        ]))
        .unwrap();
        let found = resolve_proxy_hosts(&proxies, Duration::from_secs(5)).await;
        assert!(found.contains_key(&("localhost".to_string(), 3128)));
        // https proxies keep their name and need no lookup.
        assert_eq!(found.len(), 1);
        let missing =
            env_proxies(env_of(&[("HTTP_PROXY", "http://nonexistent.invalid:3128")])).unwrap();
        let none = resolve_proxy_hosts(&missing, Duration::from_secs(5)).await;
        let got = none
            .get(&("nonexistent.invalid".to_string(), 3128))
            .unwrap();
        assert!(
            got.as_ref()
                .is_err_and(|e| e.starts_with("could not resolve proxy nonexistent.invalid")),
            "{got:?}"
        );
    }

    #[test]
    fn a_cgi_environment_has_no_proxies() {
        let env = env_of(&[
            ("REQUEST_METHOD", "GET"),
            ("HTTP_PROXY", "http://attacker:1"),
        ]);
        assert!(env_proxies(env).unwrap().is_empty());
    }

    #[test]
    fn the_first_variable_set_wins_even_when_empty() {
        let got = |pairs: &'static [(&'static str, &'static str)]| {
            env_proxies(env_of(pairs))
                .unwrap()
                .iter()
                .map(|p| (p.for_https, p.url.host_str().unwrap().to_string()))
                .collect::<Vec<_>>()
        };
        // An empty HTTP_PROXY hides http_proxy and falls to ALL_PROXY.
        assert_eq!(
            got(&[
                ("HTTP_PROXY", ""),
                ("http_proxy", "a.corp"),
                ("ALL_PROXY", "all.corp")
            ]),
            vec![
                (false, "all.corp".to_string()),
                (true, "all.corp".to_string())
            ]
        );
        assert_eq!(
            got(&[("HTTP_PROXY", "a.corp"), ("http_proxy", "b.corp")]),
            vec![(false, "a.corp".to_string())]
        );
        assert_eq!(
            got(&[("http_proxy", "b.corp")]),
            vec![(false, "b.corp".to_string())]
        );
    }

    /// A public flow's client with a proxy at a private address, built the
    /// way `http_client` builds it.
    fn proxied_client(
        proxy_env: &'static [(&'static str, &'static str)],
        no_proxy: &str,
        proxy_ip: &'static str,
        port: u16,
    ) -> crate::native::flow::HttpClients {
        let flow =
            flow_of(json!({ "startUrl": "https://app.example.com/", "origin": "example.com" }));
        let (proxies, names) = client_proxies(
            env_of(proxy_env),
            reqwest::NoProxy::from_string(no_proxy),
            move |_, _| Ok(ip(proxy_ip)),
        )
        .unwrap();
        // Every name resolves to loopback, which a public flow may not use.
        let resolver = ReachResolver {
            inner: Arc::new(Fixed(vec![SocketAddr::new(ip("127.0.0.1"), port)])),
            reach: Reach::Public,
            non_public: is_non_public,
            trusted: names.trusted,
            unreachable: names.unreachable,
        };
        http_client_with(None, &flow, Arc::new(resolver), proxies).unwrap()
    }

    #[tokio::test]
    async fn a_proxy_at_a_private_address_works() {
        let (port, hits) = serve(OK.to_string()).await;
        let env: &'static [(&str, &str)] = Box::leak(Box::new([(
            "HTTP_PROXY",
            Box::leak(format!("http://proxy.corp:{port}").into_boxed_str()) as &str,
        )]));
        let client = proxied_client(env, "", "127.0.0.1", port);
        let res = run_http(&client, &request("http://app.example.com/"), None, None).await;
        assert!(!res.reason.unwrap_or_default().contains("resolved to"));
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the proxy got the request"
        );
    }

    #[tokio::test]
    async fn a_target_that_bypasses_the_proxy_is_still_filtered() {
        let (port, hits) = serve(OK.to_string()).await;
        let env: &'static [(&str, &str)] = Box::leak(Box::new([(
            "HTTP_PROXY",
            Box::leak(format!("http://proxy.corp:{port}").into_boxed_str()) as &str,
        )]));
        let client = proxied_client(env, "app.example.com", "127.0.0.1", port);
        let res = run_http(
            &client,
            &request(&format!("http://app.example.com:{port}/")),
            None,
            None,
        )
        .await;
        let reason = res.reason.unwrap_or_default();
        assert!(
            reason.contains("app.example.com resolved to a non-public address (127.0.0.1)"),
            "{reason}"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_target_named_like_the_proxy_is_filtered() {
        // The proxy is pinned, so its name is not trusted: a direct request
        // to that name is an ordinary target.
        let (port, hits) = serve(OK.to_string()).await;
        let env: &'static [(&str, &str)] = Box::leak(Box::new([(
            "HTTP_PROXY",
            Box::leak(format!("http://proxy.example.com:{port}").into_boxed_str()) as &str,
        )]));
        let client = proxied_client(env, "proxy.example.com", "127.0.0.1", port);
        let res = run_http(
            &client,
            &request(&format!("http://proxy.example.com:{port}/")),
            None,
            None,
        )
        .await;
        assert!(res
            .reason
            .unwrap_or_default()
            .contains("non-public address"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_local_flow_with_a_local_proxy_and_no_proxy_still_runs() {
        let (port, hits) = serve(OK.to_string()).await;
        let flow = flow_of(json!({ "startUrl": "http://127.0.0.1:3000/", "origin": "127.0.0.1" }));
        let (proxies, names) = client_proxies(
            env_of(&[
                ("HTTPS_PROXY", "http://127.0.0.1:7890"),
                ("http_proxy", "http://127.0.0.1:7890"),
            ]),
            reqwest::NoProxy::from_string("127.0.0.1"),
            |_, _| panic!("an address needs no lookup"),
        )
        .unwrap();
        let resolver = ReachResolver {
            inner: Arc::new(SystemResolver),
            reach: Reach::Local,
            non_public: is_non_public,
            trusted: names.trusted,
            unreachable: names.unreachable,
        };
        let client = http_client_with(None, &flow, Arc::new(resolver), proxies).unwrap();
        let res = run_http(
            &client,
            &request(&format!("http://127.0.0.1:{port}/")),
            None,
            None,
        )
        .await;
        assert!(!res.reason.unwrap_or_default().contains("proxy"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn an_unreachable_proxy_fails_only_the_requests_that_use_it() {
        let (port, hits) = serve(OK.to_string()).await;
        let flow = flow_of(json!({ "startUrl": "http://127.0.0.1:3000/", "origin": "127.0.0.1" }));
        let (proxies, names) = client_proxies(
            env_of(&[("HTTP_PROXY", "http://proxy.offvpn.invalid:3128")]),
            reqwest::NoProxy::from_string("127.0.0.1"),
            |h, _| Err(format!("could not resolve proxy {h}: no such host")),
        )
        .unwrap();
        let resolver = ReachResolver {
            inner: Arc::new(SystemResolver),
            reach: Reach::Local,
            non_public: is_non_public,
            trusted: names.trusted,
            unreachable: names.unreachable,
        };
        let client = http_client_with(None, &flow, Arc::new(resolver), proxies).unwrap();
        // NO_PROXY sends this one direct, so the dead proxy does not matter.
        let res = run_http(
            &client,
            &request(&format!("http://127.0.0.1:{port}/")),
            None,
            None,
        )
        .await;
        assert!(!res.reason.unwrap_or_default().contains("proxy"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A target that would use the proxy fails with its reason.
        let res = run_http(&client, &request("http://localhost:9/"), None, None).await;
        let reason = res.reason.unwrap_or_default();
        assert!(
            reason.contains("could not resolve proxy proxy.offvpn.invalid"),
            "{reason}"
        );
        assert!(!res.drift);
    }
    #[test]
    fn a_recording_is_local_only_at_addresses_the_name_resolves_to() {
        let r = |a: &[&str]| {
            a.iter()
                .map(|s| s.parse().unwrap())
                .collect::<Vec<SocketAddr>>()
        };
        let lo = [ip("127.0.0.1"), ip("::1")];
        assert!(local_recording(
            &r(&["127.0.0.1:3000", "[::1]:3000"]),
            3000,
            &lo
        ));
        assert!(local_recording(
            &r(&["10.0.0.4:443"]),
            443,
            &[ip("10.0.0.4")]
        ));
        // A proxy on loopback: the site's DNS answers 127.0.0.1, but the
        // browser recorded the proxy's port, not the URL's.
        assert!(!local_recording(&r(&["127.0.0.1:8080"]), 3000, &lo));
        // A proxy elsewhere records an address the name does not resolve to.
        assert!(!local_recording(
            &r(&["127.0.0.1:3000"]),
            3000,
            &[ip("10.0.0.4")]
        ));
        assert!(!local_recording(&r(&["10.9.9.9:3000"]), 3000, &lo));
        // A public address on either side, or nothing recorded, is not local.
        assert!(!local_recording(
            &r(&["93.184.216.34:443"]),
            443,
            &[ip("93.184.216.34")]
        ));
        assert!(!local_recording(
            &r(&["127.0.0.1:80"]),
            80,
            &[ip("127.0.0.1"), ip("93.184.216.34")]
        ));
        assert!(!local_recording(&[], 80, &lo));
        assert!(!local_recording(&r(&["127.0.0.1:80"]), 80, &[]));
    }

    #[tokio::test]
    async fn a_name_counts_as_local_only_if_it_still_resolves_locally() {
        let lo = vec!["127.0.0.1:3000".parse().unwrap()];
        assert_eq!(
            recorded_local_host("localhost", 3000, &lo).await.as_deref(),
            Some("localhost")
        );
        // A public name reached through a local proxy records the proxy's
        // address but does not resolve to it.
        assert!(recorded_local_host("example.com", 3000, &lo)
            .await
            .is_none());
    }

    #[test]
    fn the_recorded_reach_overrides_the_start_url() {
        let local = flow_of(json!({ "startUrl": "http://lvh.me:3000/", "reach": "local" }));
        assert_eq!(Reach::of_flow(&local), Reach::Local);
        let old = flow_of(json!({ "startUrl": "http://lvh.me:3000/" }));
        assert_eq!(Reach::of_flow(&old), Reach::Public);
        assert_eq!(
            serde_json::to_value(&local).unwrap()["reach"],
            json!("local")
        );
        assert!(serde_json::to_value(&old).unwrap().get("reach").is_none());
    }

    #[tokio::test]
    async fn a_flow_recorded_against_a_name_resolving_to_loopback_runs() {
        let (port, hits) = serve(OK.to_string()).await;
        let url = format!("http://lvh.me:{port}/");
        let client_for = |flow: &crate::native::flow::Flow| {
            let resolver = ReachResolver {
                inner: Arc::new(Fixed(vec![SocketAddr::new(ip("127.0.0.1"), port)])),
                reach: Reach::of_flow(flow),
                non_public: is_non_public,
                trusted: vec![],
                unreachable: vec![],
            };
            http_client_with(None, flow, Arc::new(resolver), vec![]).unwrap()
        };
        let recorded_local = flow_of(json!({ "startUrl": url, "reach": "local" }));
        let res = run_http(&client_for(&recorded_local), &request(&url), None, None).await;
        assert!(!res.reason.unwrap_or_default().contains("resolved to"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A flow recorded against a public address is still refused when
        // the name later resolves to loopback.
        let recorded_public = flow_of(json!({ "startUrl": url }));
        let res = run_http(&client_for(&recorded_public), &request(&url), None, None).await;
        assert!(res
            .reason
            .unwrap_or_default()
            .contains("resolved to a non-public address"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
