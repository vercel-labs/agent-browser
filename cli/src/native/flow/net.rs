//! Where a flow's direct requests may land: only addresses of the class the
//! flow was recorded against, checked at connect time so the address that
//! was checked is the address that is used.

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Which addresses a flow may send to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    // IPv4 carried inside the address: mapped, compatible, NAT64, 6to4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return non_public_v4(v4);
    }
    if s[..6] == [0; 6] {
        return non_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return non_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        return non_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // fc00::/7 unique local
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local, fec0::/10 site-local (deprecated)
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0x0db8)
}

/// Check a URL host that is an IP literal; the resolver is never asked about
/// those. Names pass here and are checked when they resolve.
pub fn check_literal(url: &str, reach: Reach) -> Result<(), String> {
    check_literal_with(url, reach, is_non_public)
}

fn check_literal_with(
    url: &str,
    reach: Reach,
    non_public: fn(IpAddr) -> bool,
) -> Result<(), String> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Ok(());
    };
    let (host, ip) = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => (ip.to_string(), IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => (format!("[{ip}]"), IpAddr::V6(ip)),
        _ => return Ok(()),
    };
    if reach.admits(ip, non_public) {
        Ok(())
    } else {
        Err(reach.refusal(&host, ip))
    }
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
}

impl ReachResolver {
    pub fn system(reach: Reach) -> ReachResolver {
        ReachResolver {
            inner: Arc::new(SystemResolver),
            reach,
            non_public: is_non_public,
        }
    }
}

impl Resolve for ReachResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let (reach, non_public) = (self.reach, self.non_public);
        let inner = self.inner.resolve(name);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = inner.await?.collect();
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

    #[test]
    fn literal_checks() {
        for url in [
            "http://127.0.0.1/",
            "http://10.0.0.5:8080/x",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://0x7f.1/",
            "http://2130706433/",
        ] {
            let err = check_literal(url, Reach::Public).unwrap_err();
            assert!(err.contains("non-public address"), "{url}: {err}");
        }
        assert!(check_literal("https://8.8.8.8/", Reach::Public).is_ok());
        assert!(check_literal("https://example.com/", Reach::Public).is_ok());
        assert!(check_literal("http://127.0.0.1:3000/", Reach::Local).is_ok());
        let err = check_literal("http://8.8.8.8/", Reach::Local).unwrap_err();
        assert!(err.contains("public address"), "{err}");
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
        };
        http_client_with(None, &flow, Arc::new(resolver)).unwrap()
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
        };
        let client = http_client_with(None, &flow, Arc::new(resolver)).unwrap();
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

    #[test]
    fn host_policy_checks_literals_against_the_flow_class() {
        use crate::native::flow::HostPolicy;
        let local = HostPolicy::from_start("http://127.0.0.1:3000/");
        assert!(local.check("http://127.0.0.1:3000/api").is_ok());
        let public = HostPolicy::from_start("https://8.8.8.8/");
        assert!(public.check("https://8.8.8.8/x").is_ok());
        let mixed = HostPolicy::from_start("https://shop.example.com/");
        assert!(mixed.check("http://127.0.0.1/").is_err());
        assert!(mixed.check("https://api.example.com/").is_ok());
    }
}
