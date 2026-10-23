//! PostgreSQL via `tokio-postgres` (feature `postgres`, TLS with `postgres-tls`).
//! Equivalent of `AddNpgSql`.
//!
//! Accepts any of these connection-string forms:
//! - Npgsql: `Host=db;Port=5432;Database=app;Username=u;Password=p;SSL Mode=Require`
//! - URL: `postgres://u:p@db:5432/app?sslmode=require`
//! - libpq key/value: `host=db user=u password=p dbname=app sslmode=require`
//!
//! Opens a new connection per check (like Xabaril) and runs `SELECT 1`.
//!
//! SSL modes follow libpq / Npgsql, so self-signed production servers work as they do
//! from .NET:
//!
//! | mode            | TLS                 | certificate check |
//! |-----------------|---------------------|-------------------|
//! | `Disable`       | no                  | –                 |
//! | `Prefer`/`Allow`| if server offers it | none              |
//! | `Require`       | yes                 | none (unless a root certificate is given → as `VerifyCA`) |
//! | `VerifyCA`      | yes                 | chain             |
//! | `VerifyFull`    | yes                 | chain + hostname  |
//!
//! Verification uses `Root Certificate=/path/ca.pem` (`sslrootcert` in URL/libpq form)
//! when given, otherwise the OS trust store. `Require`/`Verify*` need the `postgres-tls`
//! feature. [`PostgresCheck::with_tls_config`] overrides all of this.

use std::borrow::Cow;
use std::pin::pin;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::{Either, select};
use tokio_postgres::config::{Config, SslMode};
use tokio_postgres::tls::{MakeTlsConnect, TlsConnect};
use tokio_postgres::{NoTls, Socket};

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

/// Certificate verification level (the part of `sslmode` tokio-postgres doesn't model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verify {
    /// Encrypt only (`prefer`, `require`).
    #[default]
    None,
    /// Chain must lead to a trusted root (`verify-ca`).
    Ca,
    /// Chain + hostname (`verify-full`).
    Full,
}

/// A parsed connection string.
#[derive(Debug, Clone)]
pub struct PgConnectionOptions {
    pub config: Config,
    pub verify: Verify,
    /// PEM file with the CA to trust (`Root Certificate` / `sslrootcert`).
    pub root_cert: Option<String>,
}

pub struct PostgresCheck {
    config: Config,
    query: Cow<'static, str>,
    #[cfg(feature = "postgres-tls")]
    tls: Option<tokio_postgres_rustls::MakeRustlsConnect>,
}

impl PostgresCheck {
    /// Parse an Npgsql, URL or libpq key/value connection string.
    pub fn new(connection_string: &str) -> Result<Self, BoxError> {
        Self::from_options(parse_connection_string(connection_string)?)
    }

    pub fn from_options(options: PgConnectionOptions) -> Result<Self, BoxError> {
        let PgConnectionOptions {
            config,
            mut verify,
            root_cert,
        } = options;
        let mode = config.get_ssl_mode();
        // libpq: `require` with a root certificate behaves like `verify-ca`.
        if mode == SslMode::Require && verify == Verify::None && root_cert.is_some() {
            verify = Verify::Ca;
        }

        #[cfg(feature = "postgres-tls")]
        let tls = {
            use crate::tls;
            let roots = || -> Result<_, BoxError> {
                match &root_cert {
                    Some(path) => tls::roots_from_pem(
                        &std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?,
                    ),
                    None => tls::native_roots(),
                }
            };
            let client_config = match (mode, verify) {
                (SslMode::Disable, _) => None,
                (_, Verify::None) => Some(tls::insecure_config()?),
                (_, Verify::Ca) => Some(tls::ca_only_config(roots()?)?),
                (_, Verify::Full) => Some(tls::config_with_roots(roots()?)?),
            };
            client_config.map(tokio_postgres_rustls::MakeRustlsConnect::new)
        };
        #[cfg(not(feature = "postgres-tls"))]
        {
            let _ = root_cert;
            if mode == SslMode::Require || verify != Verify::None {
                return Err(
                    "SSL Mode Require/VerifyCA/VerifyFull needs the `postgres-tls` feature".into(),
                );
            }
        }

        Ok(Self {
            config,
            query: Cow::Borrowed("SELECT 1"),
            #[cfg(feature = "postgres-tls")]
            tls,
        })
    }

    /// Use your own rustls config (see [`crate::tls`]); applies unless the mode is `Disable`.
    #[cfg(feature = "postgres-tls")]
    pub fn with_tls_config(mut self, config: crate::tls::ClientConfig) -> Self {
        if self.config.get_ssl_mode() != SslMode::Disable {
            self.tls = Some(tokio_postgres_rustls::MakeRustlsConnect::new(config));
        }
        self
    }

    /// Replace the default `SELECT 1`.
    pub fn with_query(mut self, query: impl Into<Cow<'static, str>>) -> Self {
        self.query = query.into();
        self
    }
}

#[async_trait]
impl HealthCheck for PostgresCheck {
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        #[cfg(feature = "postgres-tls")]
        if let Some(tls) = &self.tls {
            run(&self.config, tls.clone(), &self.query).await?;
            return Ok(HealthCheckResult::healthy());
        }
        run(&self.config, NoTls, &self.query).await?;
        Ok(HealthCheckResult::healthy())
    }
}

async fn run<T>(config: &Config, tls: T, query: &str) -> Result<(), BoxError>
where
    T: MakeTlsConnect<Socket>,
    T::Stream: Send,
    T::TlsConnect: Send,
    <T::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    let (client, connection) = config.connect(tls).await?;
    // Drive the connection and the query together; no task spawn needed.
    match select(pin!(client.simple_query(query)), pin!(connection)).await {
        Either::Left((result, _)) => {
            result?;
            Ok(())
        }
        Either::Right((result, _)) => {
            result?;
            Err("connection closed before the query completed".into())
        }
    }
}

/// Parse Npgsql (`Key=Value;...`), URL or libpq key/value.
pub fn parse_connection_string(s: &str) -> Result<PgConnectionOptions, BoxError> {
    let s = s.trim();
    let is_url = s.starts_with("postgres://") || s.starts_with("postgresql://");
    let is_npgsql = !is_url && (s.contains(';') || s.starts_with(|c: char| c.is_ascii_uppercase()));
    if is_npgsql {
        return parse_npgsql(s);
    }
    // tokio-postgres knows only disable/prefer/require and no sslrootcert:
    // take those two out, parse the rest, then put the mode back.
    let (rest, sslmode, root_cert) = if is_url {
        extract_url(s)
    } else {
        extract_keyvalue(s)?
    };
    let mut config = Config::from_str(&rest)?;
    let mut verify = Verify::None;
    if let Some(mode) = sslmode {
        let (m, v) = map_sslmode(&mode)?;
        config.ssl_mode(m);
        verify = v;
    }
    Ok(PgConnectionOptions {
        config,
        verify,
        root_cert,
    })
}

fn map_sslmode(value: &str) -> Result<(SslMode, Verify), BoxError> {
    let v: String = value
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    Ok(match v.as_str() {
        "disable" => (SslMode::Disable, Verify::None),
        "allow" | "prefer" => (SslMode::Prefer, Verify::None),
        "require" => (SslMode::Require, Verify::None),
        "verifyca" => (SslMode::Require, Verify::Ca),
        "verifyfull" => (SslMode::Require, Verify::Full),
        _ => return Err(format!("unsupported SSL mode: {value}").into()),
    })
}

fn parse_npgsql(s: &str) -> Result<PgConnectionOptions, BoxError> {
    let mut config = Config::new();
    let mut hosts: Vec<(String, Option<u16>)> = Vec::new();
    let mut port: Option<u16> = None;
    let mut verify = Verify::None;
    let mut root_cert = None;
    for (key, value) in split_ado(s)? {
        let key: String = key
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        match key.as_str() {
            "host" | "server" => {
                for h in value.split(',').map(str::trim).filter(|h| !h.is_empty()) {
                    match h.rsplit_once(':') {
                        Some((host, p)) if !host.contains(':') && p.parse::<u16>().is_ok() => {
                            hosts.push((host.to_owned(), Some(p.parse()?)));
                        }
                        _ => hosts.push((h.to_owned(), None)),
                    }
                }
            }
            "port" => port = Some(value.parse()?),
            "database" | "db" => {
                config.dbname(&value);
            }
            "username" | "userid" | "user" | "uid" => {
                config.user(&value);
            }
            "password" | "pwd" | "psw" => {
                config.password(&value);
            }
            "applicationname" => {
                config.application_name(&value);
            }
            "timeout" => {
                config.connect_timeout(Duration::from_secs(value.parse()?));
            }
            "sslmode" => {
                let (m, v) = map_sslmode(&value)?;
                config.ssl_mode(m);
                verify = v;
            }
            "rootcertificate" | "sslrootcert" => root_cert = Some(value),
            // Trust Server Certificate, Pooling, Command Timeout, ... don't change a probe.
            _ => {}
        }
    }
    if hosts.is_empty() {
        return Err("connection string has no Host".into());
    }
    for (host, p) in hosts {
        config.host(&host);
        config.port(p.or(port).unwrap_or(5432));
    }
    Ok(PgConnectionOptions {
        config,
        verify,
        root_cert,
    })
}

/// Split `k=v;k2='v;2';k3="a""b"` into pairs (ADO.NET quoting rules).
fn split_ado(s: &str) -> Result<Vec<(String, String)>, BoxError> {
    let mut pairs = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ';') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut value = String::new();
        match chars.peek().copied() {
            Some(q @ ('"' | '\'')) => {
                chars.next();
                loop {
                    match chars.next() {
                        Some(c) if c == q && chars.peek() == Some(&q) => {
                            chars.next();
                            value.push(q);
                        }
                        Some(c) if c == q => break,
                        Some(c) => value.push(c),
                        None => return Err(format!("unterminated quote in value of {key}").into()),
                    }
                }
                chars.by_ref().take_while(|c| *c != ';').for_each(drop);
            }
            _ => {
                value = chars.by_ref().take_while(|c| *c != ';').collect::<String>();
                value = value.trim().to_owned();
            }
        }
        pairs.push((key.trim().to_owned(), value));
    }
    Ok(pairs)
}

/// Remove `sslmode` / `sslrootcert` from a URL query string.
fn extract_url(s: &str) -> (String, Option<String>, Option<String>) {
    let Some((base, query)) = s.split_once('?') else {
        return (s.to_owned(), None, None);
    };
    let (mut mode, mut root) = (None, None);
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| match kv.split_once('=') {
            Some(("sslmode", v)) => {
                mode = Some(percent_decode(v));
                false
            }
            Some(("sslrootcert", v)) => {
                root = Some(percent_decode(v));
                false
            }
            _ => true,
        })
        .collect();
    let rest = if kept.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", kept.join("&"))
    };
    (rest, mode, root)
}

/// Remove `sslmode` / `sslrootcert` from a libpq key/value string.
fn extract_keyvalue(s: &str) -> Result<(String, Option<String>, Option<String>), BoxError> {
    let (mut mode, mut root) = (None, None);
    let mut kept = String::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut raw = String::new();
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            raw.push(c);
            chars.next();
        }
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == '=') {
            raw.push(chars.next().unwrap_or('='));
        }
        let mut value = String::new();
        if chars.peek() == Some(&'\'') {
            raw.push(chars.next().unwrap_or('\''));
            loop {
                match chars.next() {
                    Some('\\') => {
                        let c = chars.next().ok_or("dangling escape")?;
                        raw.push('\\');
                        raw.push(c);
                        value.push(c);
                    }
                    Some('\'') => {
                        raw.push('\'');
                        break;
                    }
                    Some(c) => {
                        raw.push(c);
                        value.push(c);
                    }
                    None => return Err(format!("unterminated quote in value of {key}").into()),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                raw.push(c);
                value.push(c);
                chars.next();
            }
        }
        match key.as_str() {
            "sslmode" => mode = Some(value),
            "sslrootcert" => root = Some(value),
            _ => {
                kept.push_str(&raw);
                kept.push(' ');
            }
        }
    }
    Ok((kept.trim_end().to_owned(), mode, root))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_postgres::config::Host;

    fn hosts(c: &Config) -> Vec<String> {
        c.get_hosts()
            .iter()
            .map(|h| match h {
                Host::Tcp(s) => s.clone(),
                #[allow(unreachable_patterns)]
                _ => "<unix>".into(),
            })
            .collect()
    }

    #[test]
    fn npgsql_basic() {
        let o = parse_connection_string(
            "Host=db.local;Port=6432;Database=app;Username=svc;Password='p;w''d';SSL Mode=Require;Trust Server Certificate=true;Pooling=true",
        )
        .unwrap();
        let c = &o.config;
        assert_eq!(hosts(c), ["db.local"]);
        assert_eq!(c.get_ports(), [6432]);
        assert_eq!(c.get_dbname(), Some("app"));
        assert_eq!(c.get_user(), Some("svc"));
        assert_eq!(c.get_password(), Some(&b"p;w'd"[..]));
        assert_eq!(
            (c.get_ssl_mode(), o.verify),
            (SslMode::Require, Verify::None)
        );
    }

    #[test]
    fn npgsql_hosts_ports_and_verify() {
        let o = parse_connection_string(
            "Server=a,b:5433;Port=6000;User ID=u;SslMode=VerifyFull;Root Certificate=/etc/ca.pem",
        )
        .unwrap();
        assert_eq!(hosts(&o.config), ["a", "b"]);
        assert_eq!(o.config.get_ports(), [6000, 5433]);
        assert_eq!(o.verify, Verify::Full);
        assert_eq!(o.root_cert.as_deref(), Some("/etc/ca.pem"));

        let o = parse_connection_string("Host=a;SSL Mode=Verify-CA").unwrap();
        assert_eq!(o.config.get_ports(), [5432]);
        assert_eq!(o.verify, Verify::Ca);
    }

    #[test]
    fn url_with_verify_modes() {
        let o = parse_connection_string(
            "postgres://u:p@h:5555/d?application_name=x&sslmode=verify-full&sslrootcert=%2Fetc%2Fca.pem",
        )
        .unwrap();
        assert_eq!(
            (hosts(&o.config), o.config.get_ports()),
            (vec!["h".to_owned()], &[5555][..])
        );
        assert_eq!(
            (o.config.get_ssl_mode(), o.verify),
            (SslMode::Require, Verify::Full)
        );
        assert_eq!(o.root_cert.as_deref(), Some("/etc/ca.pem"));
        assert_eq!(o.config.get_application_name(), Some("x"));
    }

    #[test]
    fn keyvalue_with_verify_modes() {
        let o = parse_connection_string(
            "host=h user=u password='a b\\'c' dbname=d sslmode=verify-ca sslrootcert='/my ca.pem'",
        )
        .unwrap();
        assert_eq!(o.config.get_dbname(), Some("d"));
        assert_eq!(o.config.get_password(), Some(&b"a b'c"[..]));
        assert_eq!(
            (o.config.get_ssl_mode(), o.verify),
            (SslMode::Require, Verify::Ca)
        );
        assert_eq!(o.root_cert.as_deref(), Some("/my ca.pem"));

        let o = parse_connection_string("host=h sslmode=disable").unwrap();
        assert_eq!(o.config.get_ssl_mode(), SslMode::Disable);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_connection_string("Database=x;Username=u").is_err());
        assert!(parse_connection_string("Host=h;SSL Mode=Bogus").is_err());
        assert!(parse_connection_string("Host=h;Password='open").is_err());
        assert!(parse_connection_string("postgres://h/d?sslmode=bogus").is_err());
    }

    #[cfg(not(feature = "postgres-tls"))]
    #[test]
    fn require_needs_tls_feature() {
        assert!(PostgresCheck::new("Host=h;SSL Mode=Require").is_err());
        assert!(PostgresCheck::new("Host=h;SSL Mode=Prefer").is_ok());
    }
}
